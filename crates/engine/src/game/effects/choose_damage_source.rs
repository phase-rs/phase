use crate::game::filter::FilterContext;
use crate::game::game_object::ObjectIncarnationRefExt;
use crate::game::targeting;
use crate::types::ability::{
    Chooser, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::zones::Zone;

/// CR 608.2c + CR 608.2e + CR 609.7a: unsupported actors must not become controller choices.
pub(crate) fn validate_inline_chooser(chooser: Option<&Chooser>) -> Result<(), EffectError> {
    match chooser {
        None | Some(Chooser::Controller) => Ok(()),
        Some(Chooser::Opponent) => Err(EffectError::InvalidParam(
            "unsupported inline damage-source chooser: Opponent".into(),
        )),
        Some(Chooser::OwningPlayer) => Err(EffectError::InvalidParam(
            "unsupported inline damage-source chooser: OwningPlayer".into(),
        )),
    }
}

/// CR 609.7a: choose from the complete typed, exact Source domain.
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
    let source_filter = match &ability.effect {
        Effect::ChooseDamageSource { source_filter } => source_filter.clone(),
        _ => {
            return Err(EffectError::InvalidParam(
                "expected ChooseDamageSource effect".to_string(),
            ))
        }
    };

    if ability.context.damage_source_choice.is_some() {
        return Ok(());
    }
    open_source_choice_owned(state, ability, &source_filter, events, declarations)?;
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// CR 608.2h + CR 609.7a: refresh only this source's actual departure, with
/// the completed permanent spell's lawful successor as the sole identity change.
pub(crate) fn refresh_selected_source(
    state: &GameState,
    source: &crate::types::game_state::TriggerSourceContext,
) -> crate::types::game_state::TriggerSourceContext {
    let mut source = source.clone();
    for record in &state.zone_changes_this_turn {
        let Some(exit) = record.trigger_source_context() else {
            continue;
        };
        if exit.identity != source.identity {
            continue;
        }
        source = exit.clone();
        // Successor rebinding is performed only by record_zone_change while
        // the actual completing Spell entry is still its authority. A bare
        // Stack-to-Battlefield ledger row cannot authorize that identity change.
    }
    source
}

/// CR 609.7a/b: exact identity and the later consumer's property predicate are separate.
pub(crate) fn selected_source_matches(
    state: &GameState,
    selected: &crate::types::game_state::TriggerSourceContext,
    damage: Option<&crate::types::game_state::TriggerSourceContext>,
    source_id: crate::types::identifiers::ObjectId,
) -> bool {
    let selected = refresh_selected_source(state, selected);
    if selected.identity.reference.object_id != source_id {
        return false;
    }
    damage.map_or_else(
        || selected.identity.reference.is_current(state),
        |damage| selected.identity.reference == damage.identity.reference,
    )
}

/// CR 609.7a: capture one exact referred object without guessing a new birth.
fn capture_reference(
    state: &GameState,
    reference: crate::types::identifiers::ObjectIncarnationRef,
) -> Option<crate::types::game_state::TriggerSourceContext> {
    if reference.is_current(state) {
        return state
            .objects
            .get(&reference.object_id)
            .map(|object| crate::game::triggers::trigger_source_context_for_latch(state, object));
    }
    state
        .zone_changes_this_turn
        .iter()
        .rev()
        .filter_map(|record| record.trigger_source_context())
        .find(|source| source.identity.reference == reference)
        .cloned()
}

fn referred_by_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    out: &mut Vec<crate::types::game_state::TriggerSourceContext>,
) {
    referred_by_ability_with(ability, out, &mut |reference| {
        capture_reference(state, reference)
    });
}

/// Share the typed reference closure with canonical old-prompt recovery. The
/// caller resolves exact references from its owned live/exit authority; this
/// traversal never treats an arbitrary selector as a referred occurrence.
pub(crate) fn referred_by_ability_with(
    ability: &ResolvedAbility,
    out: &mut Vec<crate::types::game_state::TriggerSourceContext>,
    capture: &mut impl FnMut(
        crate::types::identifiers::ObjectIncarnationRef,
    ) -> Option<crate::types::game_state::TriggerSourceContext>,
) {
    if let Some(source) = &ability.trigger_source {
        out.push(source.clone());
    }
    if let Some(incarnation) = ability.source_incarnation {
        out.extend(capture(
            crate::types::identifiers::ObjectIncarnationRef::of(ability.source_id, incarnation),
        ));
    }
    for reference in ability
        .target_pins
        .iter()
        .flatten()
        .chain(ability.target_incarnations.iter())
    {
        out.extend(capture(*reference));
    }
    if let Some(choice) = &ability.context.damage_source_choice {
        out.extend(choice.source.clone());
    }
    for snapshot in ability
        .cost_paid_object
        .iter()
        .chain(ability.effect_context_object.iter())
        .chain(ability.amassed_army_object.iter())
        .chain(
            ability
                .cost_paid_objects
                .iter()
                .filter_map(|record| record.snapshot()),
        )
    {
        out.extend(capture(
            crate::types::identifiers::ObjectIncarnationRef::of(
                snapshot.object_id,
                snapshot.incarnation,
            ),
        ));
    }
    for reference in ability
        .context
        .granting_object
        .iter()
        .chain(ability.context.boon_trigger_batch_objects.iter())
        .chain(ability.context.exile_until_batch.iter())
        .chain(
            ability
                .context
                .forwarded_result_context
                .iter()
                .flat_map(|result| result.object_incarnations.iter()),
        )
    {
        out.extend(capture(*reference));
    }
    // CR 609.7a: announced references include coin-branch target custody,
    // even before the branch that will execute has been determined.
    for (_, child) in ability.target_children() {
        referred_by_ability_with(child, out, capture);
    }
}

/// CR 609.7a: lawful Source domain, independent of damage capability or targeting.
pub(crate) fn damage_source_options(
    state: &GameState,
    ability: &ResolvedAbility,
    source_filter: &crate::types::ability::TargetFilter,
) -> Vec<crate::types::game_state::TriggerSourceContext> {
    use crate::types::game_state::StackEntryKind;
    let mut options = Vec::new();
    for id in targeting::zone_object_ids(state, Zone::Battlefield)
        .into_iter()
        .chain(state.command_zone.iter().copied())
    {
        if let Some(object) = state.objects.get(&id) {
            if object.zone != Zone::Command || !object.face_down {
                options.push(crate::game::triggers::trigger_source_context_for_latch(
                    state, object,
                ));
            }
        }
    }
    for entry in state.stack.iter().chain(state.resolving_stack_entry.iter()) {
        match &entry.kind {
            StackEntryKind::Spell { ability, .. } => {
                if let Some(object) = state.objects.get(&entry.id) {
                    options.push(crate::game::triggers::trigger_source_context_for_latch(
                        state, object,
                    ));
                }
                if let Some(ability) = ability {
                    referred_by_ability(state, ability, &mut options);
                }
            }
            StackEntryKind::ActivatedAbility { ability, .. }
            | StackEntryKind::TriggeredAbility { ability, .. } => {
                referred_by_ability(state, ability, &mut options)
            }
            StackEntryKind::KeywordAction { .. } => {}
            StackEntryKind::CombatDamage { assignments, .. } => {
                for assignment in assignments {
                    options.extend(capture_reference(state, assignment.source));
                }
            }
        }
    }
    referred_by_ability(state, ability, &mut options);
    for frame in state.resolution_stack.iter() {
        match frame {
            crate::types::resolution::ResolutionFrame::AbilityContinuation(frame) => {
                referred_by_ability(state, &frame.pending.chain, &mut options);
            }
            crate::types::resolution::ResolutionFrame::CoinFlip(frame) => {
                if let Some(choice) = &frame.branch_context.damage_source_choice {
                    options.extend(choice.source.clone());
                }
            }
            _ => {}
        }
    }
    for trigger in &state.delayed_triggers {
        referred_by_ability(state, &trigger.ability, &mut options);
    }
    for definition in state.pending_damage_replacements.iter().chain(
        state
            .objects
            .values()
            .flat_map(|object| object.replacement_definitions.iter_all()),
    ) {
        if definition.is_consumed {
            continue;
        }
        options.extend(definition.selected_damage_source.clone());
        if let Some(runtime) = definition
            .runtime_execute
            .and_then(|id| usize::try_from(id.0).ok())
            .and_then(|index| state.replacement_continuations.get(index))
        {
            referred_by_ability(state, runtime, &mut options);
        }
    }
    if let Some(pending) = &state.pending_replacement {
        options.extend(pending.damage_source_context.as_deref().cloned());
    }
    let ctx = FilterContext::from_source_with_controller(ability.source_id, ability.controller);
    options.retain(|source| {
        crate::game::filter::matches_target_filter_on_exact_source(
            state,
            source,
            source_filter,
            &ctx,
        )
    });
    options.sort_by_key(|source| source.identity.reference);
    options.dedup_by_key(|source| source.identity.reference);
    options
}

/// One prompt and its private candidate set are installed atomically on their continuation.
pub(crate) fn open_source_choice_owned(
    state: &mut GameState,
    ability: &ResolvedAbility,
    source_filter: &crate::types::ability::TargetFilter,
    _events: &mut Vec<GameEvent>,
    declarations: super::AdmittedInstructionResult<'_>,
) -> Result<(), EffectError> {
    use crate::types::game_state::{ChosenDamageSource, PendingContinuation, PromptSourceBinding};
    match &ability.effect {
        Effect::PreventDamage {
            damage_source_filter: Some(TargetFilter::ChosenDamageSource { chooser, .. }),
            ..
        }
        | Effect::CreateDamageReplacement {
            source_filter: Some(TargetFilter::ChosenDamageSource { chooser, .. }),
            ..
        } => validate_inline_chooser(chooser.as_ref())?,
        _ => {}
    }
    let candidates = damage_source_options(state, ability, source_filter);
    let mut resumed = ability.clone();
    if candidates.is_empty() {
        resumed.context.damage_source_choice = Some(Box::new(ChosenDamageSource {
            source: None,
            candidate_filter: source_filter.clone(),
        }));
        // Transfer the entire owned chain just as a nonempty prompt does.
        // Resolving it recursively here would let the caller run its tail twice.
        state.park_ability_continuation(PendingContinuation::new_owned(
            Box::new(resumed),
            state,
            declarations,
        )?);
        return Ok(());
    }
    let options = candidates
        .iter()
        .map(PromptSourceBinding::from_trigger_source)
        .collect();
    let mut pending = PendingContinuation::new_owned(Box::new(resumed), state, declarations)?;
    pending.damage_source_candidates = candidates;
    state.park_ability_continuation(pending);
    state.waiting_for = WaitingFor::DamageSourceChoice {
        player: ability.controller,
        source_filter: source_filter.clone(),
        options,
    };
    Ok(())
}
