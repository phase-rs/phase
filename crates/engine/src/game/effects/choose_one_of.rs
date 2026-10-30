use std::collections::HashSet;

use crate::game::ability_utils::build_resolved_from_def;
use crate::game::players;
use crate::types::ability::{
    AbilityDefinition, ChooseOneOfKind, Effect, EffectError, EffectKind, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, ParentTargetOccurrences, PendingChooseOneOf, ResolvingTriggerContext, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::AppliedReplacementKey;

/// CR 701.55a-b + CR 608.2d: Prompt the instructed player to choose one
/// branch at resolution. The branch itself is not pre-validated for
/// possibility; the chosen instructions perform as much as possible.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    super::with_instruction_result_entry(
        state,
        ability,
        crate::types::ability::InstructionResultEntry::Independent,
        |state, executable, owner| resolve_owned(state, executable, events, owner),
    )
}

pub(crate) fn resolve_owned(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    declarations: super::AdmittedInstructionResult<'_>,
) -> Result<(), EffectError> {
    let (chooser, branches) = match &ability.effect {
        Effect::ChooseOneOf {
            chooser, branches, ..
        } => (chooser, branches.clone()),
        _ => return Err(EffectError::MissingParam("ChooseOneOf".to_string())),
    };

    if branches.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ChooseOneOf,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    let players = choosing_players(state, ability, chooser);
    if players.is_empty() {
        // CR 608.2d: A branch choice must be made by an eligible player. An
        // empty chooser set means the effect cannot legally begin — fail loud
        // instead of silently resolving nothing (issue #927 class).
        return Err(EffectError::InvalidParam(format!(
            "ChooseOneOf: no eligible player for chooser {chooser:?}"
        )));
    }
    // CR 608.2 (run boon-blockers-20261010): capture the live trigger-event
    // context ONCE at prompt time — the pause below destroys
    // resolution-scoped context, and at branch time the event is already None.
    let trigger_context = ResolvingTriggerContext::capture(state);
    let return_result_occurrence = if let Some(ticket) = &ability.context.counter_removal_result {
        if !super::counter_removal_binding_is_valid(state, ticket)
            || ticket.process != ability.return_result_process
            || !super::instruction_result_program_projects_to(declarations.0, ability)
        {
            return Err(EffectError::InvalidParam(
                "generated counter choice producer authority".into(),
            ));
        }
        Some(ticket.occurrence)
    } else {
        super::retained_return_result_occurrence(state, ability, declarations)?
    };
    prompt_next(
        state,
        PromptRequest {
            return_result_occurrence,
            expected_return_result_process: return_result_occurrence
                .and(ability.return_result_process),
            controller: ability.controller,
            source_id: ability.source_id,
            branches,
            parent_occurrences: ParentTargetOccurrences::of(ability),
            context: ability.context.clone(),
            continuation: ability.sub_ability.clone(),
            replacement_applied: ability.replacement_applied.clone(),
            players,
            trigger_context,
        },
    );

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseOneOf,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

pub(crate) struct PromptRequest {
    pub return_result_occurrence: Option<crate::types::game_state::ReturnResultOccurrenceId>,
    pub expected_return_result_process: Option<crate::types::game_state::ReturnResultProcessId>,
    pub controller: PlayerId,
    pub source_id: ObjectId,
    pub branches: Vec<AbilityDefinition>,
    pub parent_occurrences: ParentTargetOccurrences,
    pub context: crate::types::ability::SpellContext,
    pub continuation: Option<Box<ResolvedAbility>>,
    pub replacement_applied: HashSet<AppliedReplacementKey>,
    pub players: Vec<PlayerId>,
    pub trigger_context: Option<ResolvingTriggerContext>,
}

pub(crate) fn prompt_next(state: &mut GameState, request: PromptRequest) {
    let PromptRequest {
        return_result_occurrence,
        expected_return_result_process,
        controller,
        source_id,
        branches,
        parent_occurrences,
        context,
        continuation,
        replacement_applied,
        mut players,
        trigger_context,
    } = request;
    let Some(player) = players.first().copied() else {
        return;
    };
    players.remove(0);
    let branch_descriptions = branch_descriptions(&branches);
    state.waiting_for = WaitingFor::ChooseOneOfBranch {
        return_result_occurrence,
        expected_return_result_process,
        player,
        controller,
        source_id,
        branches,
        branch_descriptions,
        parent_occurrences,
        context,
        continuation,
        replacement_applied,
        remaining_players: players,
        // CR 608.2: thread the STORED prompt-time context — never
        // fresh-capture here (`prompt_next` is also reached from
        // `resume_pending` at priority time, where a fresh capture returns
        // None and would discard the carry).
        trigger_context,
    };
    // `priority_player` routing to the chooser is owned by the centralized
    // post-apply sync (`public_state::sync_priority_player_from_waiting_for`),
    // which maps `WaitingFor::ChooseOneOfBranch { player, .. }` through
    // `turn_control::authorized_submitter_for_player` (CR 608.2d).
}

pub(crate) fn validate_pending_result_owner(
    state: &GameState,
    pending: &PendingChooseOneOf,
) -> Result<(), EffectError> {
    selection_program_owner(
        state,
        &BranchSelection {
            return_result_occurrence: pending.return_result_occurrence,
            expected_return_result_process: pending.expected_return_result_process,
            player: pending
                .remaining_players
                .first()
                .copied()
                .unwrap_or(pending.controller),
            controller: pending.controller,
            source_id: pending.source_id,
            branches: pending.branches.clone(),
            parent_occurrences: pending.parent_occurrences.clone(),
            context: (*pending.context).clone(),
            continuation: pending.continuation.clone(),
            replacement_applied: pending.replacement_applied.clone(),
            remaining_players: pending.remaining_players.clone(),
            trigger_context: pending.trigger_context.clone(),
            index: 0,
        },
    )?;
    for branch in &pending.branches {
        let mut body = build_resolved_from_def(branch, pending.source_id, pending.controller);
        body.bind_return_result_process(pending.expected_return_result_process);
        super::validate_selected_instruction_result_owner(
            state,
            &body,
            pending.return_result_occurrence,
        )?;
    }
    Ok(())
}

pub(crate) fn validate_waiting_result_owner(
    state: &GameState,
    waiting: &WaitingFor,
) -> Result<(), EffectError> {
    let WaitingFor::ChooseOneOfBranch {
        return_result_occurrence,
        expected_return_result_process,
        player,
        controller,
        source_id,
        branches,
        parent_occurrences,
        context,
        continuation,
        replacement_applied,
        remaining_players,
        trigger_context,
        ..
    } = waiting
    else {
        return Ok(());
    };
    selection_program_owner(
        state,
        &BranchSelection {
            return_result_occurrence: *return_result_occurrence,
            expected_return_result_process: *expected_return_result_process,
            player: *player,
            controller: *controller,
            source_id: *source_id,
            branches: branches.clone(),
            parent_occurrences: parent_occurrences.clone(),
            context: context.clone(),
            continuation: continuation.clone(),
            replacement_applied: replacement_applied.clone(),
            remaining_players: remaining_players.clone(),
            trigger_context: trigger_context.clone(),
            index: 0,
        },
    )
    .map(|_| ())
}

pub(crate) fn resume_pending(state: &mut GameState, _events: &mut Vec<GameEvent>) {
    if !matches!(state.waiting_for, WaitingFor::Priority { .. })
        || state.active_choose_one_of().is_none()
    {
        return;
    }
    let Some(pending) = state
        .take_active_choose_one_of()
        .expect("choose-one-of resume may consume only its active frame")
    else {
        return;
    };
    prompt_next(
        state,
        PromptRequest {
            return_result_occurrence: pending.return_result_occurrence,
            expected_return_result_process: pending.expected_return_result_process,
            controller: pending.controller,
            source_id: pending.source_id,
            branches: pending.branches,
            parent_occurrences: pending.parent_occurrences,
            context: *pending.context,
            continuation: pending.continuation,
            replacement_applied: pending.replacement_applied,
            players: pending.remaining_players,
            // CR 608.2: thread the frame's STORED context (never recapture —
            // at priority time a fresh capture returns None).
            trigger_context: pending.trigger_context,
        },
    );
}

pub(crate) struct BranchSelection {
    pub return_result_occurrence: Option<crate::types::game_state::ReturnResultOccurrenceId>,
    pub expected_return_result_process: Option<crate::types::game_state::ReturnResultProcessId>,
    pub player: PlayerId,
    pub controller: PlayerId,
    pub source_id: ObjectId,
    pub branches: Vec<AbilityDefinition>,
    pub parent_occurrences: ParentTargetOccurrences,
    pub context: crate::types::ability::SpellContext,
    pub continuation: Option<Box<ResolvedAbility>>,
    pub replacement_applied: HashSet<AppliedReplacementKey>,
    pub remaining_players: Vec<PlayerId>,
    pub trigger_context: Option<ResolvingTriggerContext>,
    pub index: usize,
}

/// The selected executable and its source/announcement inputs are bound by
/// one pure constructor shared with current/legacy program authentication.
pub(crate) fn project_selected_branch(
    parent: &ResolvedAbility,
    branch: &AbilityDefinition,
    player: PlayerId,
) -> ResolvedAbility {
    let mut resolved =
        crate::game::ability_utils::build_resolved_from_def_with_targets_and_chain_root(
            branch,
            parent.source_id,
            parent.controller,
            parent.targets.clone(),
            parent.context.chain_root_targets.clone(),
        );
    resolved.inherit_coin_target_inputs(parent);
    resolved.bind_return_result_process(parent.return_result_process);
    resolved.context = parent.context.clone();
    if matches!(resolved.effect, Effect::PutCounter { .. }) {
        resolved.context.counter_removal_result = None;
    }
    // CR 400.7: the branch inherits the parent's OCCURRENCES, pins included.
    resolved.replace_target_occurrences(parent.target_occurrences());
    resolved.set_replacement_applied_recursive(parent.replacement_applied.clone());
    resolved.set_scoped_player_recursive(player);
    // CR 603.2 + CR 608.2c (run boon-blockers-20261010): skip the chooser-target
    // push when the SELECTED branch is event-context-bound (`target: None` with
    // an `affected: TriggeringSource` carrier — the boon-fired keyword-choice
    // shape, e.g. Swiftspear's Teachings' "it gains your choice of prowess or
    // haste"). The unconditional push fills `ability.targets`, which defeats
    // the `affected: TriggeringSource` early-return in effect.rs (gated on
    // `ability.targets.is_empty()`) and misbinds the grant to
    // `SpecificPlayer(chooser)` instead of the triggering object. Every other
    // branch keeps the push byte-identically — ordinary `target: Some`
    // branches, `affected: None`/`Controller`/broadcast recipients (the push
    // is their recipient), and non-context `affected` — including the
    // villainous-choice branches the push was introduced for (de2e0db063),
    // which never carry `TriggeringSource`.
    let branch_is_event_context_bound = matches!(
        branch.effect.as_ref(),
        Effect::GenericEffect {
            target: None,
            static_abilities,
            ..
        } if static_abilities
            .iter()
            .any(|s| matches!(s.affected, Some(TargetFilter::TriggeringSource)))
    );
    if !branch_is_event_context_bound
        && !resolved
            .targets
            .iter()
            .any(|target| matches!(target, TargetRef::Player(pid) if *pid == player))
    {
        resolved.push_target(TargetRef::Player(player));
    }

    super::reset_fresh_producer_entry(&mut resolved);
    resolved
}

pub(crate) fn selection_program_owner(
    state: &GameState,
    selection: &BranchSelection,
) -> Result<
    Option<(
        ResolvedAbility,
        crate::types::ability::ResultProducerEnvironment,
        ResolvedAbility,
    )>,
    EffectError,
> {
    let held = crate::types::game_state::retained_result_waits(state).into_iter().any(|waiting| {
        matches!(waiting, WaitingFor::ChooseOneOfBranch {
            return_result_occurrence, expected_return_result_process, player, controller,
            source_id, branches, parent_occurrences, context, continuation,
            replacement_applied, remaining_players, trigger_context, ..
        } if *return_result_occurrence == selection.return_result_occurrence
            && *expected_return_result_process == selection.expected_return_result_process
            && *player == selection.player && *controller == selection.controller && *source_id == selection.source_id
            && *branches == selection.branches && *parent_occurrences == selection.parent_occurrences
            && *context == selection.context && *continuation == selection.continuation
            && *replacement_applied == selection.replacement_applied
            && *remaining_players == selection.remaining_players && *trigger_context == selection.trigger_context)
    }) || state.resolution_stack.iter().any(|frame| matches!(frame,
        crate::types::resolution::ResolutionFrame::ChooseOneOf(pending)
            if pending.return_result_occurrence == selection.return_result_occurrence
                && pending.expected_return_result_process == selection.expected_return_result_process
                && pending.source_id == selection.source_id && pending.controller == selection.controller
                && pending.branches == selection.branches && pending.parent_occurrences == selection.parent_occurrences
                && *pending.context == selection.context && pending.continuation == selection.continuation
                && pending.replacement_applied == selection.replacement_applied
                && pending.remaining_players == selection.remaining_players && pending.trigger_context == selection.trigger_context));
    let needs_owner = selection.return_result_occurrence.is_some()
        || selection.context.counter_removal_result.is_some()
        || selection.branches.iter().any(|definition| {
            let body =
                build_resolved_from_def(definition, selection.source_id, selection.controller);
            matches!(
                crate::types::ability::classify_instruction_result_invocation(
                    &body,
                    crate::types::ability::InstructionResultEntry::SelectedBody,
                    None
                ),
                Ok((
                    crate::types::ability::InstructionResultProcess::Imported { .. },
                    _
                ))
            )
        })
        || selection
            .continuation
            .as_deref()
            .is_some_and(crate::types::game_state::has_return_result_metadata);
    if !needs_owner {
        return Ok(None);
    }
    if !held {
        return Err(EffectError::InvalidParam(
            "selected candidate disagrees with its actual held branch latch".into(),
        ));
    }
    let frame = crate::types::game_state::checked_return_result_frame(
        state,
        selection.return_result_occurrence,
        crate::types::game_state::ReturnResultFrameAccess::LegacyCompatible(
            selection.expected_return_result_process,
        ),
    )
    .map_err(EffectError::InvalidParam)?;
    let program = frame.admitted_program.as_deref().ok_or_else(|| {
        EffectError::InvalidParam(
            "selected instruction-result owner has no recovered admitted program".into(),
        )
    })?;
    let (_, declarations) = crate::types::ability::classify_instruction_result_invocation(
        program,
        crate::types::ability::InstructionResultEntry::Independent,
        None,
    )?;
    if frame.declarations.as_ref() != Some(&declarations)
        || program.return_result_process != frame.process
    {
        return Err(EffectError::InvalidParam(
            "selected instruction-result program declaration mismatch".into(),
        ));
    }
    state.validate_replacement_program(program)?;
    let mut parents = Vec::new();
    let mut admit_parent = |candidate: &ResolvedAbility| {
        let Effect::ChooseOneOf {
            chooser, branches, ..
        } = &candidate.effect
        else {
            return false;
        };
        if branches == &selection.branches
            && candidate.source_id == selection.source_id
            && candidate.controller == selection.controller
            && candidate.target_occurrences() == selection.parent_occurrences.occurrences()
            && candidate.context == selection.context
            && candidate.sub_ability == selection.continuation
            && candidate.replacement_applied == selection.replacement_applied
            && choosing_players(state, candidate, chooser).contains(&selection.player)
        {
            if !parents
                .iter()
                .any(|known| super::instruction_result_projection_eq(known, candidate))
            {
                parents.push(candidate.clone());
            }
        }
        false
    };
    let mut cursor = ResolvedAbility::new(
        Effect::ChooseOneOf {
            kind: ChooseOneOfKind::Ordinary,
            chooser: crate::types::ability::PlayerFilter::Controller,
            branches: selection.branches.clone(),
        },
        Vec::new(),
        selection.source_id,
        selection.controller,
    );
    cursor.bind_return_result_process(selection.expected_return_result_process);
    cursor.context = selection.context.clone();
    cursor.replace_target_occurrences(selection.parent_occurrences.occurrences());
    cursor.sub_ability = selection.continuation.clone();
    cursor.replacement_applied = selection.replacement_applied.clone();
    if let Some(ticket) = &selection.context.counter_removal_result {
        let occurrences = selection.parent_occurrences.occurrences();
        if let [(TargetRef::Object(id), Some(pin))] = occurrences.as_slice() {
            if *id == pin.object_id {
                let mut scopes = vec![None];
                super::visit_instruction_result_program_projections(program, None, &mut |node| {
                    if !scopes.contains(&node.scoped_player) {
                        scopes.push(node.scoped_player);
                    }
                    false
                });
                for scope in [
                    selection.context.accepting_player,
                    Some(selection.controller),
                ] {
                    if !scopes.contains(&scope) {
                        scopes.push(scope);
                    }
                }
                // The hint supplies traversal axes; only complete admitted
                // inputs and the held instruction-time latch supply authority.
                for scope in scopes {
                    let mut hint = cursor.clone();
                    hint.scoped_player = scope;
                    super::visit_generated_counter_program_projections(
                        program,
                        &hint,
                        state,
                        &mut |original| {
                            if !matches!(original.effect, Effect::ChooseCounterAdjustment { .. })
                                || original.optional
                                || original.optional_for.is_some()
                                || original.unless_pay.is_some()
                                || original.player_scope.is_some()
                            {
                                return false;
                            }
                            let delegated = super::choose_counter_adjustment::delegated_choice(
                                original,
                                *pin,
                                Some(ticket.clone()),
                                selection.branches.clone(),
                            );
                            if let Some(candidate) =
                                super::choose_counter_adjustment::project_latched_choice(
                                    original, &delegated,
                                )
                            {
                                admit_parent(&candidate);
                            }
                            false
                        },
                    );
                }
            }
        }
    } else {
        super::visit_instruction_result_program_projections_for_cursor(
            program,
            &cursor,
            &mut admit_parent,
        );
    }
    if parents.len() > 1 {
        return Err(EffectError::InvalidParam(
            "selected operation has conflicting admitted parent inputs".into(),
        ));
    }
    let parent = parents.pop().ok_or_else(|| {
        EffectError::InvalidParam(
            "selected instruction-result prompt is not owned by its admitted program".into(),
        )
    })?;
    state.validate_replacement_program(&parent)?;
    Ok(Some((program.clone(), declarations, parent)))
}

pub(crate) fn resolve_branch(
    state: &mut GameState,
    selection: BranchSelection,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if selection.index >= selection.branches.len() {
        return Err(EffectError::InvalidParam(
            "ChooseOneOf branch index out of range".into(),
        ));
    }
    for branch in &selection.branches {
        let mut body = build_resolved_from_def(branch, selection.source_id, selection.controller);
        body.bind_return_result_process(selection.expected_return_result_process);
        super::validate_selected_instruction_result_owner(
            state,
            &body,
            selection.return_result_occurrence,
        )?;
    }
    let owner = selection_program_owner(state, &selection)?;
    let input = selected_operation_input(state, &selection)?;
    state.waiting_for = WaitingFor::Priority {
        player: selection.player,
    };
    state.priority_player = selection.player;
    let occurrence = selection.return_result_occurrence;
    super::with_inherited_return_result_occurrence(state, occurrence, |state| {
        resolve_branch_in_occurrence(state, selection, events, owner, input)
    })
}

fn resolve_branch_in_occurrence(
    state: &mut GameState,
    selection: BranchSelection,
    events: &mut Vec<GameEvent>,
    owner: Option<(
        ResolvedAbility,
        crate::types::ability::ResultProducerEnvironment,
        ResolvedAbility,
    )>,
    input: Option<ResolvedAbility>,
) -> Result<(), EffectError> {
    let BranchSelection {
        return_result_occurrence,
        expected_return_result_process,
        player,
        controller,
        source_id,
        branches,
        parent_occurrences,
        context,
        mut continuation,
        replacement_applied,
        remaining_players,
        trigger_context,
        index,
    } = selection;
    let Some(branch) = branches.get(index) else {
        return Err(EffectError::InvalidParam(format!(
            "ChooseOneOf branch index {index} out of range"
        )));
    };
    let local_branch = matches!(
        crate::types::ability::classify_instruction_result_invocation(
            &build_resolved_from_def(branch, source_id, controller),
            crate::types::ability::InstructionResultEntry::SelectedBody,
            None
        )
        .map(|(summary, _)| summary)?,
        crate::types::ability::InstructionResultProcess::Local { .. }
    );
    let is_final_chooser = remaining_players.is_empty();
    if let Some(binding) = &context.counter_removal_result {
        let operation_matches = match branch.effect.as_ref() {
            Effect::RemoveCounter {
                counter_type: Some(kind),
                target: crate::types::ability::TargetFilter::ParentTarget,
                ..
            }
            | Effect::PutCounter {
                counter_type: kind,
                target: crate::types::ability::TargetFilter::ParentTarget,
                ..
            } => binding.counters.matches(kind),
            _ => false,
        };
        let occurrences = parent_occurrences.occurrences();
        let pin_matches = matches!(occurrences.as_slice(), [(TargetRef::Object(id), Some(pin))]
            if Some(*pin) == binding.recipient && *id == pin.object_id);
        if !operation_matches
            || !pin_matches
            || source_id != binding.source.reference.object_id
            || !super::counter_removal_binding_is_valid(state, binding)
            || !is_final_chooser
            || branch.sub_ability.is_some()
            || branch.else_ability.is_some()
        {
            return Err(EffectError::InvalidParam(
                "counter-adjustment branch authority".into(),
            ));
        }
    }

    if !is_final_chooser {
        state.push_choose_one_of(PendingChooseOneOf {
            return_result_occurrence,
            expected_return_result_process,
            controller,
            source_id,
            branches: branches.clone(),
            parent_occurrences: parent_occurrences.clone(),
            context: Box::new(context.clone()),
            continuation: continuation.clone(),
            replacement_applied: replacement_applied.clone(),
            remaining_players,
            // CR 608.2: copy the STORED prompt-time context forward (never
            // re-capture — the event is already None at branch time).
            trigger_context: trigger_context.clone(),
        });
    }

    let mut parent = input
        .or_else(|| owner.as_ref().map(|(_, _, parent)| parent.clone()))
        .unwrap_or_else(|| {
            let mut parent = ResolvedAbility::new(Effect::NoOp, Vec::new(), source_id, controller);
            parent.bind_return_result_process(expected_return_result_process);
            parent.context = context.clone();
            parent.replace_target_occurrences(parent_occurrences.occurrences());
            parent.replacement_applied = replacement_applied.clone();
            parent
        });
    parent.context = context;
    let mut resolved = project_selected_branch(&parent, branch, player);

    // CR 608.2c + CR 701.55d: Instructions after a multi-player branch choice
    // run once, after the final chooser's selected branch. Keeping the runtime
    // continuation in the typed choice carrier preserves its exact resolved
    // context without converting it back into an AbilityDefinition.
    let outer_tail = if is_final_chooser && local_branch {
        continuation.take()
    } else {
        None
    };
    if is_final_chooser {
        if let Some(continuation) = continuation {
            crate::game::ability_utils::append_to_sub_chain(&mut resolved, *continuation);
        }
    }

    // CR 608.2: replay the carried prompt-time context around branch
    // resolution (the ChooseZoneChoice mirror) so
    // `resolve_event_context_target(TriggeringSource)` answers exactly what
    // it would have answered without the pause; `None`-carry is a
    // byte-identical no-op and the restore bounds the window to this branch.
    let trigger_snapshot = trigger_context
        .as_ref()
        .map(|ctx| super::super::triggers::push_resolving_trigger_context(state, ctx));
    let child_boundary = state.resolution_stack.capture_child_boundary();
    let outer_pending = outer_tail
        .map(|tail| match owner.as_ref() {
            Some((program, declarations, _)) => {
                crate::types::game_state::PendingContinuation::new_owned(
                    tail,
                    state,
                    (program, declarations),
                )
            }
            None => Ok(crate::types::game_state::PendingContinuation::new(
                tail, state,
            )),
        })
        .transpose()?;
    let result = (|| {
        if !local_branch {
            if let Some((program, declarations, _)) = &owner {
                super::resolve_carrier_owned_ability_chain(
                    state,
                    &resolved,
                    events,
                    1,
                    (program, declarations),
                )?;
            } else {
                super::resolve_selected_ability_chain(state, &resolved, events, 1)?;
            }
        } else {
            super::resolve_selected_ability_chain(state, &resolved, events, 1)?;
        }
        if let Some(pending) = outer_pending {
            if super::waits_for_resolution_choice(&state.waiting_for)
                || state.resolution_stack.capture_child_boundary() > child_boundary
            {
                state
                    .insert_ability_continuation_parent_at_child_boundary(pending, child_boundary)
                    .map_err(|error| EffectError::InvalidParam(error.to_string()))?;
            } else {
                super::restore_continuation_trigger_firing(state, pending.trigger_firing);
                if let Some((program, declarations, _)) = &owner {
                    super::resolve_carrier_owned_ability_chain(
                        state,
                        &pending.chain,
                        events,
                        1,
                        (program, declarations),
                    )?;
                } else {
                    super::resolve_resume_owned_ability_chain(state, &pending.chain, events, 1)?;
                }
            }
        }
        Ok::<_, EffectError>(())
    })();
    if let Some(snapshot) = trigger_snapshot {
        super::super::triggers::restore_trigger_event_context(state, snapshot);
    }
    result?;
    resume_pending(state, events);
    // NOTE: the token-choice applied seed is intentionally NOT cleared here.
    // A branch may stash a token-bearing sub-ability into `pending_continuation`
    // (effects/mod.rs) that drains only later, from the ChooseBranch handler at
    // `engine_resolution_choices.rs` via `drain_pending_continuation`. Clearing
    // here — just because `waiting_for` is momentarily back at Priority — would
    // wipe the seed before that stashed token sub-ability proposes, re-prompting
    // the originating token-choice replacement (issue #4886, review #3). The
    // seed is cleared at true full-drain in `drain_pending_continuation`
    // (Priority + no ability-continuation or repeat-for frame).
    Ok(())
}

/// A selected row can be Unused yet still carry X, cast or source inputs.
/// Its immediate typed iteration parent retains those inputs independently of
/// result ownership; no ambient result frame is borrowed for this purpose.
fn selected_operation_input(
    state: &GameState,
    selection: &BranchSelection,
) -> Result<Option<ResolvedAbility>, EffectError> {
    let ballot = state
        .active_vote_ballot()
        .map(|vote| {
            super::vote_carrier_admitted_program(state, vote)?;
            super::vote::current_ballot_body(state, vote)
        })
        .transpose()?;
    let parent = if let Some(die) = state.resolution_stack.active_die_roll() {
        super::roll_die::pending_die_admitted_program(state, die)?;
        die.retained_input
            .as_deref()
            .map(|input| (die.return_result_occurrence, input))
    } else {
        ballot
            .as_ref()
            .zip(state.active_vote_ballot())
            .map(|(body, vote)| (vote.return_result_occurrence, body))
    };
    let Some((occurrence, parent)) = parent else {
        return Ok(None);
    };
    if parent.source_id != selection.source_id {
        return Ok(None);
    }
    if (occurrence != selection.return_result_occurrence
        || parent.return_result_process != selection.expected_return_result_process)
        && (selection.return_result_occurrence.is_some()
            || selection.expected_return_result_process.is_some())
    {
        return Ok(None);
    }
    let mut cursor = ResolvedAbility::new(
        Effect::ChooseOneOf {
            kind: ChooseOneOfKind::Ordinary,
            chooser: crate::types::ability::PlayerFilter::Controller,
            branches: selection.branches.clone(),
        },
        Vec::new(),
        selection.source_id,
        selection.controller,
    );
    cursor.context = selection.context.clone();
    cursor.scoped_player = Some(selection.player);
    let mut candidates = Vec::new();
    super::visit_instruction_result_program_projections_for_cursor(
        parent,
        &cursor,
        &mut |candidate| {
            let mut candidate = candidate.clone();
            if selection.return_result_occurrence.is_none()
                && selection.expected_return_result_process.is_none()
            {
                if !matches!(
                    crate::types::ability::classify_instruction_result_invocation(
                        &candidate,
                        crate::types::ability::InstructionResultEntry::SelectedBody,
                        None
                    ),
                    Ok((crate::types::ability::InstructionResultProcess::Unused, _))
                ) {
                    return false;
                }
                candidate.bind_return_result_process(None);
            }
            if matches!(&candidate.effect, Effect::ChooseOneOf { chooser, branches, .. }
            if branches == &selection.branches && choosing_players(state, &candidate, chooser).contains(&selection.player))
                && candidate.source_id == selection.source_id
                && candidate.controller == selection.controller
                && candidate.context == selection.context
                && candidate.target_occurrences() == selection.parent_occurrences.occurrences()
                && candidate.sub_ability == selection.continuation
                && candidate.replacement_applied == selection.replacement_applied
                && !candidates
                    .iter()
                    .any(|known| super::instruction_result_projection_eq(known, &candidate))
            {
                candidates.push(candidate);
            }
            false
        },
    );
    if candidates.len() > 1 {
        return Err(EffectError::InvalidParam(
            "selected operation has conflicting retained inputs".into(),
        ));
    }
    Ok(candidates.pop())
}

fn choosing_players(
    state: &GameState,
    ability: &ResolvedAbility,
    chooser: &crate::types::ability::PlayerFilter,
) -> Vec<PlayerId> {
    use crate::types::ability::PlayerFilter;

    // CR 701.55c vs CR 608.2d: only the printed villainous-choice instruction
    // ("… faces a villainous choice — A, or B") is doubled by
    // `GrantsExtraVillainousChoice`. An ordinary "choice of" / "choose one"
    // resolves once no matter how many Valeyards are out — the pre-fix
    // authority doubled every `ChooseOneOf` unconditionally.
    let villainous = matches!(
        &ability.effect,
        Effect::ChooseOneOf {
            kind: ChooseOneOfKind::Villainous,
            ..
        }
    );
    let expand = |state: &GameState, players: Vec<PlayerId>| {
        if villainous {
            expand_extra_villainous_instances(state, players)
        } else {
            players
        }
    };

    let apnap = players::apnap_order(state);

    // CR 608.2c + CR 108.3 + CR 109.4: Three chooser filters are anchored to
    // resolution-scoped state that `matches_player_scope` cannot see (it carries
    // no `ResolvedAbility`): `ChosenPlayer` reads the player chosen earlier this
    // resolution from `ability.chosen_players`; `ParentObjectTargetOwner` reads
    // the owner of the ability's first object target (CR 108.3); and
    // `ParentObjectTargetController` reads its controller (CR 109.4) — the chooser
    // for "that creature's controller faces a villainous choice" (Hunted by The
    // Family), where the targeted creature's controller (not owner) makes the
    // choice and the two differ for a stolen creature. Resolve them here — this
    // is the one caller that has the ability in scope — and order the result in
    // APNAP (CR 701.55d). All filter out eliminated players (CR 104.3a — a player
    // who loses leaves the game and can no longer be a chooser) and yield a
    // single chooser, which is correct for the villainous-choice patterns these
    // power (The Master, This Is How It Ends, Hunted by The Family).
    let anchored: Option<PlayerId> = match chooser {
        PlayerFilter::ChosenPlayer { index } => {
            ability.chosen_players.get(*index as usize).copied()
        }
        PlayerFilter::ParentObjectTargetOwner => {
            crate::game::ability_utils::parent_target_owner(ability, state)
        }
        PlayerFilter::ParentObjectTargetController => {
            crate::game::ability_utils::parent_target_controller(ability, state)
        }
        _ => None,
    };
    if let Some(player) = anchored {
        let alive = state
            .players
            .iter()
            .any(|p| p.id == player && !p.is_eliminated);
        let players = if alive { vec![player] } else { Vec::new() };
        return expand(state, players);
    }

    let targeted: Vec<PlayerId> = ability
        .targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Player(player) => Some(*player),
            _ => None,
        })
        .filter(|player| {
            super::matches_player_scope(
                state,
                *player,
                chooser,
                ability.controller,
                ability.source_id,
            )
        })
        .collect();

    if !targeted.is_empty() {
        let players = apnap
            .into_iter()
            .filter(|player| targeted.contains(player))
            .collect();
        return expand(state, players);
    }

    let players = apnap
        .into_iter()
        .filter(|player| {
            super::matches_player_scope(
                state,
                *player,
                chooser,
                ability.controller,
                ability.source_id,
            )
        })
        .collect();
    expand(state, players)
}

/// CR 701.55c: Count the number of ADDITIONAL villainous-choice instances a
/// `facing` player must perform — one per active `GrantsExtraVillainousChoice`
/// static on a battlefield permanent controlled by an OPPONENT of that player
/// (The Valeyard — "If an opponent would face a villainous choice, they face
/// that choice an additional time."). Returns the additional count (default 0),
/// not `1 + count`: the base instance is already represented by the player's
/// single occurrence in the facing-player list.
///
/// This is the controller-inverted mirror of `vote::votes_per_session_for`
/// (CR 701.38d), where the source is controlled by the voting player themselves;
/// here the source (the Valeyard) is controlled by the facing player's opponent.
fn villainous_extra_instances_for(state: &GameState, facing: PlayerId) -> u32 {
    use crate::game::functioning_abilities::active_static_definitions;
    use crate::types::statics::StaticMode;

    let mut extras: u32 = 0;
    for &src_id in state.battlefield.iter() {
        let Some(obj) = state.objects.get(&src_id) else {
            continue;
        };
        if !players::is_opponent(state, facing, obj.controller) {
            continue;
        }
        for s in active_static_definitions(state, obj) {
            if matches!(s.mode, StaticMode::GrantsExtraVillainousChoice) {
                extras = extras.saturating_add(1);
            }
        }
    }
    extras
}

/// CR 701.55c + CR 701.55d: Expand a facing-player list so each player appears
/// once per total instance of the villainous choice they must face — their base
/// occurrence plus `villainous_extra_instances_for` additional copies, inserted
/// consecutively so APNAP order (CR 701.55d) across distinct players is
/// preserved while each player resolves all of their instances one at a time
/// (CR 701.55c).
fn expand_extra_villainous_instances(state: &GameState, players: Vec<PlayerId>) -> Vec<PlayerId> {
    let mut expanded = Vec::with_capacity(players.len());
    for p in players {
        expanded.push(p);
        for _ in 0..villainous_extra_instances_for(state, p) {
            expanded.push(p);
        }
    }
    expanded
}

fn branch_descriptions(branches: &[AbilityDefinition]) -> Vec<String> {
    branches
        .iter()
        .enumerate()
        .map(|(index, branch)| {
            if let Some(description) = branch
                .description
                .as_ref()
                .map(|text| text.trim())
                .filter(|text| !text.is_empty())
            {
                return description.to_string();
            }
            if let Effect::Token { name, .. } = &*branch.effect {
                return format!("Create a {name} token");
            }
            format!("Option {}", index + 1)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityKind, CardSelectionMode, Chooser, Comparator, PlayerFilter, PlayerRelation,
        PlayerScope, PtValue, QuantityExpr, QuantityRef, TargetFilter, ZoneOwner,
    };
    use crate::types::actions::GameAction;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::resolution::FrameKind;
    use crate::types::zones::Zone;
    use crate::types::PlayerId;

    #[test]
    fn held_choice_deduplicates_complete_parents_and_refuses_conflicting_inputs() {
        use crate::types::ability::ReturnResultId;
        for conflicting in [false, true] {
            let mut state = GameState::new_two_player(42);
            let source = create_object(
                &mut state,
                CardId(100),
                PlayerId(0),
                "Supplemental complete parent input".into(),
                Zone::Battlefield,
            );
            let producer = |result| {
                let effect = serde_json::from_value(serde_json::json!({
                    "type":"ChangeZoneAll", "origin":"Battlefield", "destination":"Exile",
                    "target":TargetFilter::SpecificObject { id: ObjectId(0) }
                }))
                .unwrap();
                let mut definition = AbilityDefinition::new(AbilityKind::Spell, effect);
                definition.declares_return_result = Some(ReturnResultId(result));
                definition
            };
            let mut branch = producer(8);
            branch.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    player: TargetFilter::Controller,
                    amount: QuantityExpr::Fixed { value: 3 },
                },
            )));
            let mut choice = ResolvedAbility::new(
                Effect::ChooseOneOf {
                    kind: ChooseOneOfKind::Ordinary,
                    chooser: PlayerFilter::Controller,
                    branches: vec![branch.clone(), branch],
                },
                vec![],
                source,
                PlayerId(0),
            );
            choice.chosen_x = Some(3);
            let mut root = build_resolved_from_def(&producer(7), source, PlayerId(0));
            root.sub_ability = Some(Box::new(choice.clone()));
            if conflicting {
                choice.chosen_x = Some(4);
            }
            root.else_ability = Some(Box::new(choice));
            super::super::resolve_ability_chain(&mut state, &root, &mut Vec::new(), 0).unwrap();
            assert!(matches!(
                state.waiting_for,
                WaitingFor::ChooseOneOfBranch {
                    return_result_occurrence: Some(_),
                    ..
                }
            ));
            if conflicting {
                let before = state.clone();
                let error = apply_as_current(&mut state, GameAction::ChooseBranch { index: 1 })
                    .unwrap_err()
                    .to_string();
                assert!(
                    error.contains("conflicting admitted parent inputs"),
                    "{error}"
                );
                assert_eq!(state, before);
                let value = serde_json::to_value(&state).unwrap();
                assert!(serde_json::from_value::<GameState>(value)
                    .unwrap_err()
                    .to_string()
                    .contains("conflicting admitted parent inputs"));
            } else {
                let _: GameState =
                    serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
                apply_as_current(&mut state, GameAction::ChooseBranch { index: 1 }).unwrap();
                assert_eq!(state.players[0].life, 23);
                assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
                assert!(state.stack.is_empty());
                assert!(state.resolution_stack.is_empty());
                assert!(state.return_result_frames.is_empty());
            }
        }
    }

    #[test]
    fn empty_chooser_set_fails_loudly() {
        let mut state = GameState::new_two_player(42);
        state.players[0].is_eliminated = true;
        state.players[1].is_eliminated = true;

        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![branch],
                kind: ChooseOneOfKind::Ordinary,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let err = resolve(&mut state, &ability, &mut events).unwrap_err();
        assert!(
            err.to_string().contains("no eligible player"),
            "expected chooser failure, got {err}"
        );
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ));
    }

    #[test]
    fn multi_chooser_branch_pause_drains_before_next_chooser() {
        // CR 701.55d + CR 608.2c: after player 1 chooses a branch that pauses,
        // its continuation must finish before player 2 faces the same choice.
        // The outer `ChooseOneOf` frame is the continuation's immediate parent;
        // reversing those frames would resume player 2 prematurely.
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Villainous Source".to_string(),
            Zone::Battlefield,
        );
        let choice = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Choice Card".to_string(),
            Zone::Graveyard,
        );

        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Graveyard,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Controller.into(),
                candidate_source: crate::types::ability::ZoneChoiceCandidateSource::Legacy,
                reciprocal_role: None,
                up_to: false,
                constraint: None,
                selection: CardSelectionMode::Chosen,
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ));
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Opponent,
                branches: vec![branch],
                kind: ChooseOneOfKind::Ordinary,
            },
            Vec::new(),
            source,
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 10 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        ));
        let mut events = Vec::new();

        super::resolve(&mut state, &ability, &mut events).expect("first opponent is prompted");
        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .expect("first opponent chooses the pausing branch");
        assert_eq!(
            state
                .resolution_stack
                .iter()
                .map(crate::types::resolution::ResolutionFrame::kind)
                .collect::<Vec<_>>(),
            vec![FrameKind::ChooseOneOf, FrameKind::AbilityContinuation],
            "the first branch continuation must remain above the queued second chooser"
        );
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ChooseFromZoneChoice {
                player: PlayerId(0),
                ..
            }
        ));

        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![choice],
            },
        )
        .expect("resolving the pause must drain its branch continuation");
        assert_eq!(
            state.players[0].life, 21,
            "the outer tail must not run until the paused first branch and every chooser finish"
        );
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ChooseOneOfBranch {
                player: PlayerId(2),
                remaining_players: ref rest,
                ..
            } if rest.is_empty()
        ));

        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .expect("second opponent chooses the branch after the first branch drained");
        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![choice],
            },
        )
        .expect("second branch pause resolves");
        assert_eq!(
            state.players[0].life, 32,
            "two branch gains plus exactly one outer tail after the final paused branch"
        );
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.resolution_stack.is_empty());
    }

    #[test]
    fn multi_chooser_runtime_tail_runs_once_after_final_choice() {
        // CR 701.55d + CR 608.2c: each instructed player resolves a branch in
        // APNAP order, then the instruction following the whole choice runs
        // once. It must not run after every player's branch.
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        );
        let tail = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 10 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Opponent,
                branches: vec![branch],
                kind: ChooseOneOfKind::Ordinary,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(tail);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("first opponent is prompted");
        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .expect("first opponent chooses a branch");
        assert_eq!(
            state.players[0].life, 21,
            "the outer tail cannot run before the final chooser"
        );
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ChooseOneOfBranch {
                player: PlayerId(2),
                ..
            }
        ));

        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .expect("final opponent chooses a branch");
        assert_eq!(
            state.players[0].life, 32,
            "two branch gains plus exactly one ten-life outer tail"
        );
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    #[test]
    fn token_branches_without_descriptions_get_create_labels() {
        let food = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "Food".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".into(), "Food".into()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
        );
        let treasure = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "Treasure".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".into(), "Treasure".into()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
        );
        let labels = branch_descriptions(&[food, treasure]);
        assert_eq!(
            labels,
            vec!["Create a Food token", "Create a Treasure token"]
        );
    }

    #[test]
    fn explicit_branch_descriptions_reach_waiting_for_prompt() {
        let mut state = GameState::new_two_player(42);
        let colorless = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .description("Colorless".to_string());
        let white = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .description("White".to_string());
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![colorless, white],
                kind: ChooseOneOfKind::Ordinary,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                branch_descriptions,
                ..
            } => {
                assert_eq!(
                    branch_descriptions,
                    &vec!["Colorless".to_string(), "White".to_string()]
                );
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }
    }

    #[test]
    fn chosen_player_chooser_prompts_chosen_opponent() {
        // CR 608.2c + CR 109.4: A `ChooseOneOf` whose chooser is
        // `PlayerFilter::ChosenPlayer { index: 0 }` must prompt the player
        // recorded in `ability.chosen_players[0]` (the opponent chosen earlier
        // this resolution — The Master, Gallifrey's End), not the controller.
        let mut state = GameState::new(FormatConfig::commander(), 3, 42);

        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let mut ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::ChosenPlayer { index: 0 },
                branches: vec![branch],
                kind: ChooseOneOfKind::Ordinary,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        ability.chosen_players = vec![PlayerId(2)];
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player,
                remaining_players,
                ..
            } => {
                assert_eq!(*player, PlayerId(2));
                assert!(remaining_players.is_empty());
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }
    }

    #[test]
    fn parent_object_target_owner_chooser_prompts_target_owner() {
        // CR 108.3 + CR 109.4: A `ChooseOneOf` whose chooser is
        // `ParentObjectTargetOwner` must prompt the owner of the ability's first
        // object target (This Is How It Ends — the targeted creature's owner
        // faces the villainous choice).
        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        // Create an object owned by player 2 and bind it as the parent target.
        let obj_id = ObjectId(99);
        let obj = crate::game::game_object::GameObject::new(
            obj_id,
            crate::types::identifiers::CardId(0),
            PlayerId(2),
            "Test Creature".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state.objects.insert(obj_id, obj);

        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::ParentObjectTargetOwner,
                branches: vec![branch],
                kind: ChooseOneOfKind::Ordinary,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch { player, .. } => {
                assert_eq!(*player, PlayerId(2), "owner of target should be chooser");
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }
    }

    #[test]
    fn life_lost_player_attribute_chooser_prompts_only_matching_opponents() {
        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        state.players[1].life_lost_this_turn = 3;
        state.players[2].life_lost_this_turn = 2;

        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::PlayerAttribute {
                    relation: PlayerRelation::Opponent,
                    attr: Box::new(QuantityRef::LifeLostThisTurn {
                        player: PlayerScope::ScopedPlayer,
                    }),
                    comparator: Comparator::GE,
                    value: Box::new(QuantityExpr::Fixed { value: 3 }),
                },
                branches: vec![branch],
                kind: ChooseOneOfKind::Ordinary,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player,
                remaining_players,
                ..
            } => {
                assert_eq!(*player, PlayerId(1));
                assert!(remaining_players.is_empty());
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }
    }

    /// CR 701.55c (cluster 32, Class D — The Valeyard): A
    /// `GrantsExtraVillainousChoice` static on a battlefield permanent
    /// controlled by an OPPONENT of the facing player makes that player face the
    /// choice one additional time. The facing-player list expands so the player
    /// appears twice consecutively (base + 1 extra); without the static they
    /// appear exactly once. Tests the building block
    /// (`expand_extra_villainous_instances`) via the live resolver, not a card.
    #[test]
    fn villainous_choice_doubled_when_opponent_controls_extra_instance_static() {
        // Player 1 faces the choice; player 0 controls a Valeyard-like source.
        let mut state = GameState::new(FormatConfig::commander(), 3, 42);

        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );

        // Sanity baseline: with no extra-instance source, player 1 faces the
        // choice exactly once (no remaining players queued for a re-face).
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::ParentObjectTargetOwner,
                branches: vec![branch.clone()],
                kind: ChooseOneOfKind::Villainous,
            },
            vec![TargetRef::Object(ObjectId(99))],
            ObjectId(1),
            PlayerId(0),
        );
        // Bind the parent target to an object owned by player 1.
        let obj_id = ObjectId(99);
        let target_obj = crate::game::game_object::GameObject::new(
            obj_id,
            crate::types::identifiers::CardId(0),
            PlayerId(1),
            "Faced Creature".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state.objects.insert(obj_id, target_obj);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player,
                remaining_players,
                ..
            } => {
                assert_eq!(*player, PlayerId(1));
                assert!(
                    remaining_players.is_empty(),
                    "without an extra-instance static the facing player faces the choice once"
                );
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }

        // Now add a Valeyard-like permanent controlled by player 0 (an opponent
        // of the facing player 1) carrying GrantsExtraVillainousChoice.
        let valeyard_id = ObjectId(50);
        let mut valeyard = crate::game::game_object::GameObject::new(
            valeyard_id,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "The Valeyard".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        valeyard.static_definitions.push(
            crate::types::ability::StaticDefinition::new(
                crate::types::statics::StaticMode::GrantsExtraVillainousChoice,
            )
            .affected(TargetFilter::Player),
        );
        state.objects.insert(valeyard_id, valeyard);
        state.battlefield.push_back(valeyard_id);

        let mut events2 = Vec::new();
        resolve(&mut state, &ability, &mut events2).unwrap();
        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player,
                remaining_players,
                ..
            } => {
                assert_eq!(*player, PlayerId(1));
                // CR 701.55c: the same player faces the choice one more time,
                // queued consecutively right after their first instance.
                assert_eq!(
                    remaining_players.as_slice(),
                    &[PlayerId(1)],
                    "the facing player must face the choice twice (base + 1 extra)"
                );
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }
    }

    /// CR 701.55c + CR 608.2: the second-instance continuation of a doubled
    /// villainous choice carries the SAME prompt-time trigger context — the
    /// `resume_pending` copy-forward, not a re-capture (the live event is
    /// already None at branch time). Explicitly synthetic continuation
    /// fixture: no printed card pairs a villainous instruction with a
    /// trigger-context earner, so the test seeds `current_trigger_event`
    /// directly, clears it after the first prompt (simulating the real
    /// pause, which destroys resolution-scoped context), and drives both
    /// instances through `GameAction::ChooseBranch`.
    #[test]
    fn villainous_second_instance_continuation_preserves_trigger_context() {
        let mut state = GameState::new(FormatConfig::commander(), 3, 42);

        let obj_id = ObjectId(99);
        let target_obj = crate::game::game_object::GameObject::new(
            obj_id,
            crate::types::identifiers::CardId(0),
            PlayerId(1),
            "Faced Creature".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state.objects.insert(obj_id, target_obj);

        // A Valeyard-like permanent controlled by player 0 (an opponent of
        // the facing player 1) carrying GrantsExtraVillainousChoice.
        let valeyard_id = ObjectId(50);
        let mut valeyard = crate::game::game_object::GameObject::new(
            valeyard_id,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "The Valeyard".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        valeyard.static_definitions.push(
            crate::types::ability::StaticDefinition::new(
                crate::types::statics::StaticMode::GrantsExtraVillainousChoice,
            )
            .affected(TargetFilter::Player),
        );
        state.objects.insert(valeyard_id, valeyard);
        state.battlefield.push_back(valeyard_id);

        // Seed a live trigger event; the prompt captures it.
        let event = crate::types::events::GameEvent::PhaseChanged {
            phase: crate::types::phase::Phase::Upkeep,
        };
        state.current_trigger_event = Some(event.clone());
        state.current_trigger_events = vec![event.clone()];

        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::ParentObjectTargetOwner,
                branches: vec![branch],
                kind: ChooseOneOfKind::Villainous,
            },
            vec![TargetRef::Object(ObjectId(99))],
            ObjectId(1),
            PlayerId(0),
        );

        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).expect("villainous choice prompts");
        let first_context = match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player,
                remaining_players,
                trigger_context: Some(captured),
                ..
            } => {
                assert_eq!(*player, PlayerId(1));
                assert_eq!(
                    remaining_players.as_slice(),
                    &[PlayerId(1)],
                    "the doubled choice queues its second instance up front"
                );
                assert_eq!(
                    captured.event,
                    Some(event.clone()),
                    "the first prompt captures the live trigger event"
                );
                captured.clone()
            }
            other => panic!("expected first ChooseOneOfBranch, got {other:?}"),
        };

        // Simulate the real pause: resolution-scoped context is gone by
        // branch time, so the second prompt's context can only arrive via
        // the stored copy-forward — never a re-capture.
        state.current_trigger_event = None;
        state.current_trigger_events.clear();

        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .expect("first instance answers");
        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player,
                trigger_context: Some(carried),
                ..
            } => {
                assert_eq!(*player, PlayerId(1));
                assert!(
                    carried.event.is_some(),
                    "the carried context must still hold the event after the live one cleared"
                );
                assert_eq!(
                    carried.event, first_context.event,
                    "the second instance must carry the SAME captured event, not a re-capture"
                );
            }
            other => panic!("expected second ChooseOneOfBranch, got {other:?}"),
        }

        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .expect("second instance answers");
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "both instances answered, back to priority: {:?}",
            state.waiting_for
        );
    }

    /// CR 701.55c (cluster 32, Class D): an extra-instance source controlled by
    /// the FACING player themselves (not an opponent) grants no extra instance —
    /// the static reads "if an OPPONENT would face a villainous choice". Guards
    /// the controller-inversion in `villainous_extra_instances_for`.
    #[test]
    fn villainous_choice_not_doubled_by_self_controlled_static() {
        let mut state = GameState::new(FormatConfig::commander(), 3, 42);

        let obj_id = ObjectId(99);
        let target_obj = crate::game::game_object::GameObject::new(
            obj_id,
            crate::types::identifiers::CardId(0),
            PlayerId(1),
            "Faced Creature".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state.objects.insert(obj_id, target_obj);

        // Source controlled by player 1 (the facing player) — must NOT count.
        let src_id = ObjectId(50);
        let mut src = crate::game::game_object::GameObject::new(
            src_id,
            crate::types::identifiers::CardId(1),
            PlayerId(1),
            "Self Valeyard".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        src.static_definitions.push(
            crate::types::ability::StaticDefinition::new(
                crate::types::statics::StaticMode::GrantsExtraVillainousChoice,
            )
            .affected(TargetFilter::Player),
        );
        state.objects.insert(src_id, src);
        state.battlefield.push_back(src_id);

        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::ParentObjectTargetOwner,
                branches: vec![branch],
                kind: ChooseOneOfKind::Villainous,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player,
                remaining_players,
                ..
            } => {
                assert_eq!(*player, PlayerId(1));
                assert!(
                    remaining_players.is_empty(),
                    "a self-controlled extra-instance static must not double the choice"
                );
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        }
    }
}
