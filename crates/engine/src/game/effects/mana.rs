use crate::game::mana_sources::mana_type_to_color;
use crate::game::quantity::{resolve_quantity, resolve_quantity_with_targets};
use crate::game::{mana_payment, mana_sources};
#[cfg(test)]
use crate::types::ability::ManaContribution;
use crate::types::ability::{
    ChoiceValue, Effect, EffectError, EffectKind, LinkedExileScope, ManaProduction,
    ManaSpendRestriction, ManaTargetRole, ManaTargetSlot, ObjectScope, QuantityExpr,
    ResolvedAbility, TargetRef,
};
use crate::types::ability::{DevotionColors, QuantityRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, ManaChoice, ManaChoiceContext, ManaChoicePrompt, MayTriggerOrigin,
    ReturnResultOccurrenceId, WaitingFor,
};
use crate::types::mana::{ManaColor, ManaRestriction, ManaType};
use crate::types::player::PlayerId;

/// CR 601.2c + CR 608.2b: A view of `ability` whose `targets` contain only the
/// target chosen for `slot`, re-validated against that slot's own filter.
/// Shared quantity resolution (`QuantityRef::TargetZoneCardCount`,
/// `LifeTotal { player: Target }`) reads "the first player target", so each role
/// must be resolved against a view holding only its own — otherwise the
/// recipient at index 0 would be read as the count source. Scoping HERE keeps
/// `game/quantity.rs` slot-agnostic, because `TargetZoneCardCount` serves many
/// non-mana cards.
///
/// Returns `None` when the role declares no filter for `slot`, when that filter
/// is a context-ref (no slot surfaced), when the index is out of range, or when
/// the chosen target is no longer legal (CR 608.2b) — callers distinguish
/// "no such role" from "role present but target illegal" via `role.filter_for`.
fn ability_scoped_to_slot(
    state: &GameState,
    ability: &ResolvedAbility,
    role: &ManaTargetRole,
    slot: ManaTargetSlot,
) -> Option<ResolvedAbility> {
    // The role index is a declared position; it is also the stored position
    // because resolution validation keeps every role of a multi-role mana in
    // place (`OccurrenceVerdict::IllegalRetained`) unless all are illegal.
    let index = role.slot_index(slot)?;
    let filter = role.filter_for(slot)?;
    let chosen = ability.targets.get(index)?;
    let legal = crate::game::targeting::validate_targets_for_ability(
        state,
        std::slice::from_ref(chosen),
        filter,
        ability,
    );
    let _kept = legal.into_iter().next()?;
    let mut scoped = ability.clone();
    scoped.project_target_occurrences(&retain_only_player_at(ability, Some(index)));
    Some(scoped)
}

/// CR 601.2c: Narrow `ability.targets` to ONE player target — the entry at
/// `keep`, or none at all when `keep` is `None` — while leaving every NON-player
/// target in place at its original position.
///
/// Scoping must be confined to the axis the shared quantity resolvers actually
/// read. `QuantityRef::TargetZoneCardCount` and `LifeTotal { player: Target }`
/// scan for the FIRST `TargetRef::Player`, so leaving two players visible is
/// what let a count read the recipient. Object-scoped production
/// (`ManaProduction::AnyCombinationOfObjectColors { scope: Target }` via
/// `object_colors_for_scope`) reads an OBJECT target from the same vec and
/// requires nothing about the player roles — clearing the whole vec would make
/// that half of the production silently produce no colors, which CR 608.2b does
/// not license: only the part that "requires information about an illegal
/// target" fails.
///
/// Returns the kept POSITIONS: scoping is an occurrence projection
/// (`ResolvedAbility::project_target_occurrences`), so each kept occurrence
/// keeps its own pin.
fn retain_only_player_at(ability: &ResolvedAbility, keep: Option<usize>) -> Vec<usize> {
    ability
        .targets
        .iter()
        .enumerate()
        .filter(|(i, t)| !matches!(t, TargetRef::Player(_)) || Some(*i) == keep)
        .map(|(i, _)| i)
        .collect()
}

/// CR 601.2c + CR 608.2b: The ability view the production COUNT resolves
/// against. FOUR distinct cases, which must not be collapsed:
///
/// 1. No count-source role declared (no role at all, or `Recipient`-only —
///    Jetfire, and every single-role mana). The count reads nothing
///    target-derived, so the unscoped ability is correct and this preserves
///    today's behavior exactly.
/// 2. The count source is a CONTEXT REF (`ScopedPlayer`, `TriggeringPlayer`,
///    …). It surfaces no target slot, so there is no chosen target to be
///    illegal — the player comes from context, exactly as the recipient path
///    does. Resolving it through the CR 608.2b branch instead would silently
///    yield 0 under a rule that does not apply, because nothing here is an
///    illegal target.
/// 3. A count source is declared, surfaces a slot, and its chosen target is
///    still legal. Narrow to that ONE player, so shared "first player target"
///    quantity resolution (`QuantityRef::TargetZoneCardCount`,
///    `LifeTotal { player: Target }`) cannot read the recipient instead.
/// 4. A count source is declared but its chosen target is no longer legal.
///    CR 608.2b: the effect "fails to determine any such information" about an
///    illegal target — so expose NO player, resolving the count to 0. Falling
///    back to the unscoped ability here would be a BUG: it still holds the
///    (legal) recipient, so the count would read the RECIPIENT's hand instead
///    of failing.
///
/// In cases 3 and 4 only the PLAYER axis is narrowed; non-player targets stay
/// put (see `retain_only_player_at`).
fn count_scoped_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    role: Option<&ManaTargetRole>,
) -> ResolvedAbility {
    // Case 1: nothing target-derived to scope.
    let Some(role) = role.filter(|r| r.count_source().is_some()) else {
        return ability.clone();
    };
    // Case 2: context-ref count source — read the player from context, mirroring
    // `mana_effect_recipient`, and expose it as the sole player target.
    if let Some(filter) = role.count_source().filter(|filter| filter.is_context_ref()) {
        let from_context = super::resolve_player_for_context_ref(state, ability, filter);
        let mut scoped = ability.clone();
        scoped.project_target_occurrences(&retain_only_player_at(ability, None));
        if let Some(player) = from_context {
            scoped.push_target(TargetRef::Player(player));
        }
        return scoped;
    }
    // Case 3, else case 4.
    ability_scoped_to_slot(state, ability, role, ManaTargetSlot::CountSource).unwrap_or_else(|| {
        let mut scoped = ability.clone();
        scoped.project_target_occurrences(&retain_only_player_at(ability, None));
        scoped
    })
}

/// CR 106.4 + CR 608.2b: Which player's mana pool receives the mana.
/// `None` means the effect declares a recipient whose chosen target is no longer
/// legal — the mana is not added to any pool ("illegal targets won't be affected
/// by parts of the effect for which they're illegal"). This is DISTINCT from a
/// role with no recipient at all (Jeska's Will, Carpet of Flowers), which
/// correctly deposits into `ability.controller`.
///
/// Shared by the immediate `resolve` path, the color-choice prompt, and the
/// prompt-completion path so all three agree on the recipient. No quantity is
/// inspected anywhere — the role states the answer.
/// CR 106.4 + CR 109.5: single recipient authority for mana-echo production
/// ("add one mana of any type that land produced"), shared by the prompt
/// origin in `resolve` and the `handle_choose_mana_effect` completion. The
/// recipient is the tap event's tapper — never the controller default — so
/// an unscoped echo observing another player's tap directs the choice and
/// the deposit to the tapping player (the sibling single-type contract:
/// P0 source observing P1's tap credits P1, not P0). `None` when no tap
/// event is live; echo produces no mana then anyway (CR 106.5).
fn echo_recipient(state: &GameState) -> Option<PlayerId> {
    state
        .current_trigger_event
        .as_ref()
        .and_then(|event| match event {
            GameEvent::TappedForMana { player_id, .. }
            | GameEvent::ManaAbilityProduced { player_id, .. } => Some(*player_id),
            _ => None,
        })
}

fn mana_effect_recipient(
    state: &GameState,
    ability: &ResolvedAbility,
    role: Option<&ManaTargetRole>,
) -> Option<PlayerId> {
    let Some(filter) = role.and_then(ManaTargetRole::recipient) else {
        // CR 106.4: no recipient role declared — the controller adds the mana.
        return Some(ability.controller);
    };
    // CR 106.4: context-ref recipients (ScopedPlayer, TriggeringPlayer,
    // ParentTargetController, chosen player) resolve via the context, not
    // `ability.targets`.
    if filter.is_context_ref() {
        return super::resolve_player_for_context_ref(state, ability, filter);
    }
    // CR 115.1 + CR 106.4: "target player adds …" (Jetfire, Ingenious
    // Scientist) — the targeted player was chosen at announcement and lives in
    // this role's OWN slot. An illegal chosen target yields `None`: the mana is
    // not deposited anywhere (CR 608.2b). Never fall back to the controller —
    // that would hand a targeted player's mana to the caster.
    let scoped = ability_scoped_to_slot(state, ability, role?, ManaTargetSlot::Recipient)?;
    scoped.targets.iter().find_map(|t| match t {
        TargetRef::Player(player_id) => Some(*player_id),
        TargetRef::Object(_) => None,
    })
}

/// Mana effect: adds mana to the recipient's mana pool (CR 106.4).
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
    super::validate_addressed_quantity_reads(state, ability)?;
    let (produced, restrictions, grants, expiry, mana_role) = match &ability.effect {
        Effect::Mana {
            produced,
            restrictions,
            grants,
            expiry,
            // CR 601.2c: `target` is a ROLE. The RECIPIENT names the player
            // whose mana pool receives the mana (CR 106.4); the COUNT SOURCE
            // names the player a `TargetZoneCardCount` / `LifeTotal` quantity
            // inside `produced` reads. Each is resolved below from its OWN
            // target slot — no quantity shape is inspected to tell them apart.
            target,
        } => (produced, restrictions, grants, *expiry, target.clone()),
        _ => return Err(EffectError::MissingParam("Produced".to_string())),
    };
    // CR 601.2c + CR 608.2b: resolve the production count against a view of the
    // ability holding ONLY the count source's own chosen target, so shared
    // "first player target" quantity resolution cannot read the recipient.
    let count_ability = count_scoped_ability(state, ability, mana_role.as_ref());
    let count_ability = &count_ability;
    // CR 605.4a: read back the acceptance decision for the occurrence that is
    // actually executing rather than re-answering CR 605.1b from a clone the
    // resolver may already have bound a context referent onto. With no accepted
    // occurrence live this is byte-for-byte the baseline raw classifier call.
    let is_triggered_mana_inline =
        crate::game::mana_abilities::is_resolving_triggered_mana(state, ability);
    // CR 608.2d: a mana-echo body ("add one mana of any type that land
    // produced") resolving from a multi-type tap event pauses for exactly one
    // chosen type even inside immediate (triggered-mana) resolution — the
    // fixed-point authority pauses for and resumes the choice (whitelisted
    // below) without a stack entry, so the inline gate must not swallow it.
    // Single-type echo yields no prompt here and falls through to the normal
    // gate, resolving with no interaction.
    let echo_pause = crate::game::mana_abilities::echo_body_owes_type_choice(
        ability,
        state.current_trigger_event.as_ref(),
    )
    .then(|| {
        crate::game::mana_abilities::mana_choice_prompt(
            &ability.effect,
            state,
            ability.source_id,
            Some(ability),
            Some(count_ability),
        )
    })
    .flatten();
    let mana_choice = echo_pause.or_else(|| {
        (!is_triggered_mana_inline)
            .then(|| {
                crate::game::mana_abilities::mana_choice_prompt(
                    &ability.effect,
                    state,
                    ability.source_id,
                    Some(ability),
                    Some(count_ability),
                )
            })
            .flatten()
    });
    if let Some(choice) = mana_choice {
        // CR 106.4: the player who *chooses* the color is the effect's named
        // recipient — for "that player adds one mana of any color they choose"
        // (Spectral Searchlight, Stadium Vendors) that is the chosen player, not
        // the controller. Resolve it here so the prompt is directed correctly;
        // `handle_choose_mana_effect` re-derives the same recipient for deposit.
        //
        // CR 608.2b: when the declared recipient's chosen target is no longer
        // legal, no player adds this mana, so there is no color for anyone to
        // choose — the mana part of the effect simply does nothing.
        //
        // CR 106.4 + CR 109.5: a mana-echo production reads its recipient from
        // the tap event's tapper through the single `echo_recipient` authority
        // below — the same authority completion uses — so prompt direction and
        // deposit agree even for unscoped echo observing another player's tap
        // (where the controller default would misdirect the choice).
        let prompt_recipient = match &ability.effect {
            Effect::Mana {
                produced: ManaProduction::TriggerEventManaType,
                ..
            } => echo_recipient(state),
            _ => mana_effect_recipient(state, ability, mana_role.as_ref()),
        };
        let Some(prompt_player) = prompt_recipient else {
            return Ok(());
        };
        state.waiting_for = WaitingFor::ChooseManaColor {
            player: prompt_player,
            return_result_occurrence: mana_choice_return_result_occurrence(
                state,
                ability,
                &choice,
                declarations,
            )?,
            choice,
            context: ManaChoiceContext::ResolvingEffect(Box::new(ability.clone())),
        };
        return Ok(());
    }

    // CR 106.3: Mana is produced by the effects of mana abilities, spells, and
    // abilities that aren't mana abilities. The source of produced mana is the
    // source of the ability or spell.
    // CR 107.1b: When X is part of a mana production quantity (rare — e.g., an
    // effect on the stack that resolved via `ResolvedAbility` and produces X mana),
    // `resolve_quantity_with_targets` threads `ability.chosen_x` through to the
    // `Variable { name: "X" }` branch of `resolve_ref`. Non-X mana production
    // (Fixed, ObjectCount, etc.) is unaffected.
    //
    // CR 605.1b + CR 106.12a: For inline `TapsForMana` triggered mana abilities
    // (Fertile Ground, Utopia Sprawl AnyOneColor), the auto-tap planner may have
    // stored a `current_triggered_mana_override` so the resolver produces the
    // planned color rather than defaulting to the first listed option.
    let mana_types = if is_triggered_mana_inline {
        match state.current_triggered_mana_override.clone() {
            Some(crate::types::game_state::ProductionOverride::SingleColor(color)) => {
                // Resolve the count from the production descriptor, then produce
                // that many units of the override color — mirrors the behavior of
                // `resolve_single_color_override` in `mana_abilities.rs`.
                let count = match mana_type_to_color(color) {
                    Some(chosen) => {
                        let bound = bind_any_one_color_choice(produced, chosen);
                        resolve_mana_types_with_contexts(
                            &bound,
                            &*state,
                            Some(ability),
                            count_ability,
                        )
                        .len()
                    }
                    None => resolve_mana_types_with_contexts(
                        produced,
                        &*state,
                        Some(ability),
                        count_ability,
                    )
                    .len(),
                };
                vec![color; count]
            }
            Some(crate::types::game_state::ProductionOverride::Combination(types)) => types,
            None => {
                resolve_mana_types_with_contexts(produced, &*state, Some(ability), count_ability)
            }
        }
    } else {
        resolve_mana_types_with_contexts(produced, &*state, Some(ability), count_ability)
    };
    let source_could_produce_two_or_more_colors =
        mana_sources::mana_production_could_produce_two_or_more_colors(
            state,
            ability.controller,
            ability.source_id,
            produced,
            Some(ability),
        );

    // Resolve restriction templates into concrete restrictions
    let concrete_restrictions = resolve_restrictions(restrictions, state, ability.source_id);

    let recipient = match produced {
        // CR 106.3 + CR 109.5: "add one mana of any type that land produced" —
        // the bonus mana goes to the player who tapped the land (the
        // `TappedForMana` event's `player_id`), not the trigger's controller.
        // Read through the single `echo_recipient` authority shared with the
        // prompt origin and the choice completion below.
        ManaProduction::TriggerEventManaType => echo_recipient(state).unwrap_or(ability.controller),
        // CR 106.4: A subject-led mana clause routes the mana to the named
        // player ("the active player adds {C}{C} …" on a Phase trigger, "that
        // player adds one mana of any color" on Spectral Searchlight).
        _ => match mana_effect_recipient(state, ability, mana_role.as_ref()) {
            Some(player) => player,
            // CR 608.2b: "Illegal targets won't be affected by parts of the
            // effect for which they're illegal." A declared recipient whose
            // chosen target is no longer legal receives nothing, and the mana is
            // NOT redirected to the controller. The count source's half of the
            // sentence already resolved above and is unaffected.
            None => {
                events.push(GameEvent::EffectResolved {
                    kind: EffectKind::from(&ability.effect),
                    source_id: ability.source_id,
                    subject: None,
                });
                return Ok(());
            }
        },
    };

    // CR 106.4: When an effect instructs a player to add mana, that mana goes
    // into that player's mana pool.
    let produced_mana = !mana_types.is_empty();
    let mut deposited = false;
    for mana_type in mana_types {
        deposited |= !mana_payment::produce_mana_with_attributes_from_source_quality(
            state,
            ability.source_id,
            mana_type,
            recipient,
            false,
            source_could_produce_two_or_more_colors,
            &concrete_restrictions,
            grants,
            expiry,
            events,
        )
        .is_empty();
    }
    record_firebending_if_marked(state, ability, produced_mana, events);
    record_triggered_ability_added_mana(state, ability, recipient, deposited);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// Visits only quantities that the current public color response re-reads.
/// The combination prompt owns its latched units; target roles and surveyed
/// production filters remain live even when the production count is latched.
pub(crate) fn for_each_mana_choice_response_quantity(
    ability: &ResolvedAbility,
    prompt: &ManaChoicePrompt,
    visit: &mut dyn FnMut(&QuantityRef),
) {
    let Effect::Mana { produced, .. } = &ability.effect else {
        return;
    };
    if matches!(prompt, ManaChoicePrompt::SingleColor { .. }) {
        produced.for_each_quantity_expr(&mut |quantity| quantity.for_each_ref(visit));
    }
    // This is the effect's shared predicate walk. It includes the recipient,
    // count-source and surveyed production filters, but no registered-later
    // mana grant or sequential continuation.
    ability.effect.for_each_predicate_quantity_ref(visit);
}

fn mana_choice_return_result_occurrence(
    state: &GameState,
    ability: &ResolvedAbility,
    prompt: &ManaChoicePrompt,
    declarations: super::AdmittedInstructionResult<'_>,
) -> Result<Option<ReturnResultOccurrenceId>, EffectError> {
    let mut reads = false;
    for_each_mana_choice_response_quantity(ability, prompt, &mut |reference| {
        reads |= reference.instruction_result_id().is_some();
    });
    let occurrence = if reads {
        Some(state.active_return_result_occurrence.ok_or_else(|| {
            EffectError::InvalidParam(
                "mana response has no selected instruction-result owner".into(),
            )
        })?)
    } else {
        None
    };
    validate_mana_choice_response_owned(state, ability, prompt, occurrence, Some(declarations))?;
    Ok(occurrence)
}

pub(crate) fn validate_mana_choice_response(
    state: &GameState,
    ability: &ResolvedAbility,
    prompt: &ManaChoicePrompt,
    occurrence: Option<ReturnResultOccurrenceId>,
) -> Result<(), EffectError> {
    validate_mana_choice_response_owned(state, ability, prompt, occurrence, None)
}

pub(crate) fn admitted_mana_response_matches_program(
    state: &GameState,
    program: &ResolvedAbility,
    ability: &ResolvedAbility,
    prompt: &ManaChoicePrompt,
) -> bool {
    let mut current = ability.clone();
    current.sub_ability = None;
    current.else_ability = None;
    if let (
        Effect::Mana {
            produced: ManaProduction::AnyCombination { count, .. },
            ..
        },
        ManaChoicePrompt::AnyCombination { .. },
    ) = (&mut current.effect, prompt)
    {
        *count = QuantityExpr::Fixed { value: 0 };
    }
    super::visit_instruction_result_program_projections_for_cursor(
        program,
        ability,
        &mut |candidate| {
            let Effect::Mana { target, .. } = &candidate.effect else {
                return false;
            };
            let mut expected = candidate.clone();
            expected.sub_ability = None;
            expected.else_ability = None;
            if let (
                Effect::Mana {
                    produced: ManaProduction::AnyCombination { count, .. },
                    ..
                },
                ManaChoicePrompt::AnyCombination { .. },
            ) = (&mut expected.effect, prompt)
            {
                *count = QuantityExpr::Fixed { value: 0 };
            }
            if !super::instruction_result_projection_eq(&expected, &current) {
                return false;
            }
            match (&candidate.effect, prompt) {
                (
                    Effect::Mana {
                        produced:
                            ManaProduction::AnyCombination {
                                count,
                                color_options,
                            },
                        ..
                    },
                    ManaChoicePrompt::AnyCombination {
                        count: latched_count,
                        options,
                    },
                ) => {
                    *latched_count > 0
                        && color_options
                            .iter()
                            .copied()
                            .map(ManaType::from)
                            .eq(options.iter().copied())
                        && if let QuantityExpr::Fixed { value } = count {
                            usize::try_from(*value).ok() == Some(*latched_count)
                        } else {
                            true
                        }
                }
                _ => {
                    let count_owner = count_scoped_ability(state, candidate, target.as_ref());
                    crate::game::mana_abilities::mana_choice_prompt(
                        &candidate.effect,
                        state,
                        candidate.source_id,
                        Some(candidate),
                        Some(&count_owner),
                    )
                    .as_ref()
                        == Some(prompt)
                }
            }
        },
    )
}

fn validate_mana_choice_response_owned(
    state: &GameState,
    ability: &ResolvedAbility,
    prompt: &ManaChoicePrompt,
    occurrence: Option<ReturnResultOccurrenceId>,
    admitted: Option<super::AdmittedInstructionResult<'_>>,
) -> Result<(), EffectError> {
    if !matches!(ability.effect, Effect::Mana { .. }) {
        return Err(EffectError::InvalidParam(
            "pending mana choice is not a mana effect".into(),
        ));
    }
    let mut failure = None;
    let mut invalid = None;
    for_each_mana_choice_response_quantity(ability, prompt, &mut |reference| {
        if let Some(result) = reference.instruction_result_id() {
            if let Err(error) = crate::types::game_state::checked_owned_return_result_frame(
                state,
                occurrence,
                ability.return_result_process,
            ) {
                failure.get_or_insert(error);
            }
            let value = occurrence
                .and_then(|owner| state.return_result_frames.get(&owner))
                .and_then(|frame| frame.results.get(&result));
            if !matches!(value, Some(crate::types::game_state::InstructionResult::ZoneChanges(records))
                if records.iter().all(crate::game::filter::departure_record_has_coherent_snapshot))
            {
                invalid.get_or_insert(result);
            }
        }
    });
    if let Some(error) = failure {
        return Err(EffectError::InvalidParam(error));
    }
    let mut addressed = false;
    for_each_mana_choice_response_quantity(ability, prompt, &mut |reference| {
        addressed |= reference.instruction_result_id().is_some();
    });
    if addressed || occurrence.is_some() {
        let frame = crate::types::game_state::checked_owned_return_result_frame(
            state,
            occurrence,
            ability.return_result_process,
        )
        .map_err(EffectError::InvalidParam)?;
        let saved_program = frame.admitted_program.as_deref().ok_or_else(|| {
            EffectError::InvalidParam("mana response has no recovered admitted program".into())
        })?;
        let (_, declarations) = crate::types::ability::classify_instruction_result_invocation(
            saved_program,
            crate::types::ability::InstructionResultEntry::Independent,
            None,
        )?;
        if frame.declarations.as_ref() != Some(&declarations)
            || saved_program.return_result_process != frame.process
        {
            return Err(EffectError::InvalidParam(
                "mana response program declaration mismatch".into(),
            ));
        }
        if let Some((program, namespace)) = admitted {
            if program != saved_program || namespace != &declarations {
                return Err(EffectError::InvalidParam("mana prompt invocation owner mismatch".into()));
            }
        } else if !crate::types::game_state::retained_result_waits(state).into_iter().any(|waiting| matches!(waiting,
            WaitingFor::ChooseManaColor { return_result_occurrence, choice, context: ManaChoiceContext::ResolvingEffect(pending), .. }
                if *return_result_occurrence == occurrence && choice == prompt && pending.as_ref() == ability))
        {
            return Err(EffectError::InvalidParam("mana response does not own the active typed prompt".into()));
        }
        let matches_program =
            admitted_mana_response_matches_program(state, saved_program, ability, prompt);
        if !matches_program {
            return Err(EffectError::InvalidParam(
                "mana response is not the admitted operation and latched prompt".into(),
            ));
        }
    }
    if let Some(result) = invalid {
        return Err(EffectError::InvalidParam(format!(
            "unavailable mana response instruction result {result:?}"
        )));
    }
    if occurrence.is_some_and(|owner| !state.return_result_frames.contains_key(&owner)) {
        return Err(EffectError::InvalidParam(
            "missing mana response instruction-result frame".into(),
        ));
    }
    Ok(())
}

/// CR 106.3 + CR 608.2d: Complete a mana-choice prompt created while a spell
/// or non-mana ability effect is resolving.
pub fn handle_choose_mana_effect(
    state: &mut GameState,
    ability: &ResolvedAbility,
    prompt_player: PlayerId,
    prompt: &ManaChoicePrompt,
    return_result_occurrence: Option<ReturnResultOccurrenceId>,
    chosen: ManaChoice,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, crate::game::engine::EngineError> {
    validate_mana_choice_response(state, ability, prompt, return_result_occurrence)
        .map_err(|error| crate::game::engine::EngineError::InvalidAction(error.to_string()))?;
    super::with_inherited_return_result_occurrence(state, return_result_occurrence, |state| {
        validate_mana_choice_response(state, ability, prompt, return_result_occurrence)
            .map_err(|error| crate::game::engine::EngineError::InvalidAction(error.to_string()))?;
        complete_mana_choice(state, ability, prompt_player, prompt, chosen, events)
    })
}

fn complete_mana_choice(
    state: &mut GameState,
    ability: &ResolvedAbility,
    prompt_player: PlayerId,
    prompt: &ManaChoicePrompt,
    chosen: ManaChoice,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, crate::game::engine::EngineError> {
    let Effect::Mana {
        produced,
        restrictions,
        grants,
        expiry,
        target,
    } = &ability.effect
    else {
        return Err(crate::game::engine::EngineError::InvalidAction(
            "Pending mana choice is not a mana effect".to_string(),
        ));
    };

    // CR 601.2c + CR 608.2b: the prompt-completion path derives the COUNT too
    // (`SingleColor` multiplies the chosen color by it), so it must read the
    // count source's own slot for exactly the same reason `resolve` does —
    // otherwise a `Both` role with a color choice counts the RECIPIENT.
    let count_ability = count_scoped_ability(state, ability, target.as_ref());
    let mana_types =
        chosen_mana_types_for_prompt(state, ability, &count_ability, produced, prompt, chosen)?;
    let source_could_produce_two_or_more_colors =
        mana_sources::mana_production_could_produce_two_or_more_colors(
            state,
            ability.controller,
            ability.source_id,
            produced,
            Some(ability),
        );
    let concrete_restrictions = resolve_restrictions(restrictions, state, ability.source_id);
    // CR 106.4: deposit the mana into the effect's named recipient's pool (the
    // same player the color prompt was directed to in `resolve`), not the
    // controller. Priority still returns to the controller below — only the mana
    // is redirected.
    //
    // CR 608.2b: `None` means the declared recipient's chosen target is no
    // longer legal — no player adds this mana, and it is NOT redirected to the
    // controller.
    //
    // CR 106.3 + CR 109.5 (mirrors `resolve` above): a mana-echo production
    // reads its recipient from the triggering tap event's tapper through the
    // single `echo_recipient` authority — the same authority the prompt
    // origin uses — so prompt direction and deposit agree even for unscoped
    // echo observing another player's tap. The tapper is latched two ways
    // so suspension cannot lose it: the live trigger event when still
    // present, and — as the final backstop — the prompt player carried in
    // `WaitingFor::ChooseManaColor` (which `resolve` directed to this same
    // recipient when the choice was offered). On every reachable input the
    // two agree; the backstop only matters if the event was cleared while
    // the choice was pending.
    let recipient: Option<PlayerId> = match produced {
        ManaProduction::TriggerEventManaType => echo_recipient(state).or(Some(prompt_player)),
        _ => mana_effect_recipient(state, ability, target.as_ref()),
    };
    let produced_mana = recipient.is_some() && !mana_types.is_empty();
    let mut deposited = false;
    if let Some(recipient) = recipient {
        for mana_type in mana_types {
            deposited |= !mana_payment::produce_mana_with_attributes_from_source_quality(
                state,
                ability.source_id,
                mana_type,
                recipient,
                false,
                source_could_produce_two_or_more_colors,
                &concrete_restrictions,
                grants,
                *expiry,
                events,
            )
            .is_empty();
        }
    }
    record_firebending_if_marked(state, ability, produced_mana, events);
    if let Some(recipient) = recipient {
        record_triggered_ability_added_mana(state, ability, recipient, deposited);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    // Priority is restored to the ability's controller exactly as before this
    // change (the recipient computed above governs only where the mana lands, a
    // player receiving mana does not thereby gain priority). Kept as prior
    // behavior rather than migrated to the active player (CR 117.3b) to stay
    // scoped to the mana-recipient fix.
    state.waiting_for = WaitingFor::Priority {
        player: ability.controller,
    };
    state.priority_player = ability.controller;
    super::drain_pending_continuation(state, events);
    Ok(state.waiting_for.clone())
}

fn record_firebending_if_marked(
    state: &mut GameState,
    ability: &ResolvedAbility,
    produced_mana: bool,
    events: &mut Vec<GameEvent>,
) {
    if !produced_mana {
        return;
    }
    let Some(MayTriggerOrigin::Keyword {
        keyword: crate::types::keywords::KeywordKind::Firebending,
    }) = ability.may_trigger_origin
    else {
        return;
    };
    crate::game::bending::record_bending(
        state,
        events,
        crate::types::events::BendingType::Fire,
        ability.source_id,
        ability.controller,
    );
}

/// CR 106.4 + CR 607.1c: the single recording authority for "this triggered
/// ability added mana this turn". Records the resolving ability's exact
/// `TriggerDefinitionRef` and actual receiving `PlayerId` when `deposited`
/// is true, meaning at least one `ManaUnit` reached a pool. Two deposit
/// routes call it: the
/// `Effect::Mana` resolver (`resolve`, `handle_choose_mana_effect`), and
/// the resolution-time `AbilityCost::EffectCost { Effect::Mana }` payment
/// in `costs::pay_ability_cost_inner`. Paying that cost carries out the
/// resolving ability's own instruction to add mana (CR 118.1 + CR 118.12 +
/// CR 608.2c), so the mana is added with that ability (Braid of Fire's
/// cumulative upkeep, CR 702.24a). A `None` ref is the ordinary
/// non-triggered case: spells, activated abilities, replacement
/// may-costs. The state-trigger builder passes `None`, so a state trigger records nothing here.
pub(crate) fn record_triggered_ability_added_mana(
    state: &mut GameState,
    ability: &ResolvedAbility,
    player: PlayerId,
    deposited: bool,
) {
    // No unit reached a pool (declined, zero-count, prevented, or no legal
    // recipient): nothing was added with this ability, so there is no record.
    if !deposited {
        return;
    }
    // Only a triggered ability carries a definition ref; any other resolving
    // ability that adds mana is outside this per-trigger record by design.
    let Some(definition_ref) = &ability.trigger_definition_ref else {
        return;
    };
    state
        .triggered_abilities_added_mana_this_turn
        .insert((definition_ref.clone(), player));
}

fn chosen_mana_types_for_prompt(
    state: &GameState,
    ability: &ResolvedAbility,
    count_ability: &ResolvedAbility,
    produced: &ManaProduction,
    prompt: &ManaChoicePrompt,
    chosen: ManaChoice,
) -> Result<Vec<ManaType>, crate::game::engine::EngineError> {
    match (prompt, chosen) {
        (ManaChoicePrompt::SingleColor { options }, ManaChoice::SingleColor(color)) => {
            if !options.contains(&color) {
                return Err(crate::game::engine::EngineError::InvalidAction(
                    "Chosen color is not among the legal options".to_string(),
                ));
            }
            // CR 106.12a: a mana-echo production ("add ONE mana of any type
            // that land produced") structurally carries no count — the variant
            // has no count field — so one choice yields exactly one mana even
            // when the triggering event produced several types. Every other
            // SingleColor production multiplies the choice by its resolved
            // count, with the chosen color bound into choice-reading counts
            // first (upstream count-binding authority, preserved here).
            let count = if matches!(produced, ManaProduction::TriggerEventManaType) {
                1
            } else {
                match mana_type_to_color(color) {
                    Some(chosen) => {
                        let bound = bind_any_one_color_choice(produced, chosen);
                        resolve_mana_types_with_contexts(
                            &bound,
                            state,
                            Some(ability),
                            count_ability,
                        )
                        .len()
                    }
                    None => resolve_mana_types_with_contexts(
                        produced,
                        state,
                        Some(ability),
                        count_ability,
                    )
                    .len(),
                }
            };
            Ok(vec![color; count])
        }
        (ManaChoicePrompt::Combination { options }, ManaChoice::Combination(combo)) => {
            if !options.iter().any(|option| option == &combo) {
                return Err(crate::game::engine::EngineError::InvalidAction(
                    "Chosen combination is not among the legal options".to_string(),
                ));
            }
            Ok(combo)
        }
        (ManaChoicePrompt::AnyCombination { count, options }, ManaChoice::Combination(combo)) => {
            if combo.len() != *count || combo.iter().any(|color| !options.contains(color)) {
                return Err(crate::game::engine::EngineError::InvalidAction(
                    "Chosen mana combination is not legal for this prompt".to_string(),
                ));
            }
            Ok(combo)
        }
        _ => Err(crate::game::engine::EngineError::InvalidAction(
            "Mana choice shape does not match the active prompt".to_string(),
        )),
    }
}

/// Resolve parse-time restriction templates into concrete `ManaRestriction` values.
/// CR 106.6: Some spells or abilities that produce mana restrict how that mana can be spent.
pub(crate) fn resolve_restrictions(
    templates: &[ManaSpendRestriction],
    state: &GameState,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<ManaRestriction> {
    templates
        .iter()
        .filter_map(|template| match template {
            ManaSpendRestriction::SpellOnly => Some(ManaRestriction::OnlyForSpell),
            ManaSpendRestriction::SpellType(t) => {
                Some(ManaRestriction::OnlyForSpellType(t.clone()))
            }
            // Preserve the historical behavior of this older template: it is
            // omitted when the source has no creature-type choice. The newer
            // `SpellOfSourceChosenColor` below deliberately differs; its
            // missing choice must make the produced mana unspendable.
            ManaSpendRestriction::ChosenCreatureType => state
                .objects
                .get(&source_id)
                .and_then(|obj| obj.chosen_creature_type())
                .map(|ct| ManaRestriction::OnlyForCreatureType(ct.to_string())),
            // CR 105.2 + CR 106.6: The spell's color must equal the mana
            // source's live chosen color. A missing source/choice is not an
            // omitted restriction; it makes this produced mana unspendable.
            ManaSpendRestriction::SpellOfSourceChosenColor => Some(
                state
                    .objects
                    .get(&source_id)
                    .and_then(|obj| obj.chosen_color())
                    .map(ManaRestriction::OnlyForSpellColor)
                    .unwrap_or(ManaRestriction::Impossible),
            ),
            // CR 106.6: Combined spell type + ability activation restriction.
            ManaSpendRestriction::SpellTypeOrAbilityActivation {
                spell_type,
                ability,
            } => Some(ManaRestriction::OnlyForTypeSpellsOrAbilities {
                spell_type: spell_type.clone(),
                ability: *ability,
            }),
            ManaSpendRestriction::ActivateOnly => Some(ManaRestriction::OnlyForActivation),
            ManaSpendRestriction::ActivateTagged(tag) => {
                Some(ManaRestriction::OnlyForTaggedActivation(*tag))
            }
            ManaSpendRestriction::XCostOnly => Some(ManaRestriction::OnlyForXCosts),
            ManaSpendRestriction::SpellWithKeywordKind(kind) => {
                Some(ManaRestriction::OnlyForSpellWithKeywordKind(*kind))
            }
            ManaSpendRestriction::SpellWithKeywordKindFromZone { kind, zone } => Some(
                ManaRestriction::OnlyForSpellWithKeywordKindFromZone(*kind, *zone),
            ),
            ManaSpendRestriction::SpellWithManaValue { comparator, value } => {
                Some(ManaRestriction::OnlyForSpellWithManaValue {
                    comparator: *comparator,
                    value: *value,
                })
            }
            // CR 106.6 + CR 107.3 + CR 202.3: Lower the disjunctive MV/X cost
            // criteria (with optional type narrowing) into the runtime gate
            // checked against `SpellMeta` by `allows_spell`.
            ManaSpendRestriction::SpellMatchingCostCriteria {
                spell_type,
                criteria,
            } => Some(ManaRestriction::OnlyForSpellMatchingCostCriteria {
                spell_type: spell_type.clone(),
                criteria: criteria.clone(),
            }),
            // CR 105.2 + CR 106.6: Lower color-count spend restrictions into the
            // runtime gate checked against `SpellMeta.color_count`.
            ManaSpendRestriction::SpellWithColorCount { comparator, count } => {
                Some(ManaRestriction::OnlyForSpellWithColorCount {
                    comparator: *comparator,
                    count: *count,
                })
            }
            ManaSpendRestriction::SpellFromZone(zs) => {
                Some(ManaRestriction::OnlyForSpellFromZone(*zs))
            }
            ManaSpendRestriction::CannotCastSpellFromZone(zone) => {
                Some(ManaRestriction::CannotCastSpellFromZone(*zone))
            }
            // CR 106.6 + CR 116.2m + CR 709.5e: Lower the door-unlock special-action
            // leaf into the runtime gate checked by `allows_special_action` when a
            // Room's unlock cost is paid through `PaymentContext::SpecialAction`.
            ManaSpendRestriction::UnlockDoor => Some(ManaRestriction::OnlyForSpecialAction(
                crate::types::mana::SpecialAction::UnlockDoor,
            )),
            // CR 106.6 + CR 708.4: Lower the face-down-cast leaf into the runtime
            // gate checked against `SpellMeta.is_face_down` by `allows_spell`. The
            // gate reads cast face-down intent (not `obj.face_down`), so it
            // correctly rejects exile-concealment casts (foretell/hideaway, whose
            // `obj.face_down = true` but which are cast face up, CR 702.143c). It is
            // fail-closed: no production path casts a spell face down, so the gate
            // never over-permits.
            ManaSpendRestriction::FaceDownSpell => Some(ManaRestriction::OnlyForFaceDownSpell),
            // CR 106.6 + CR 116.2b + CR 702.37e / CR 702.168d / CR 701.40b: Lower
            // the turn-face-up special-action leaf into the runtime gate checked by
            // `allows_special_action`. The `GameAction::TurnFaceUp` handler pays the
            // morph/disguise/manifest cost through
            // `PaymentContext::SpecialAction(TurnFaceUp)`, so this gate is live —
            // spendable for a turn-up and rejected for any other context.
            ManaSpendRestriction::TurnPermanentFaceUp => {
                Some(ManaRestriction::OnlyForSpecialAction(
                    crate::types::mana::SpecialAction::TurnFaceUp,
                ))
            }
            // CR 106.6: Disjunction — recursively lower each branch. The
            // chosen-color branch preserves its fail-closed `Impossible`; the
            // legacy chosen-creature-type branch retains its historical drop.
            ManaSpendRestriction::Any(subs) => {
                let inner = resolve_restrictions(subs, state, source_id);
                (!inner.is_empty()).then_some(ManaRestriction::OnlyForAny(inner))
            }
        })
        .collect()
}

/// Resolve a typed mana production descriptor into concrete mana units.
///
/// CR 605.3a: Mana abilities don't use the stack, so they have no `ResolvedAbility`
/// and thus no `chosen_x` — this entry point used to be the legacy path for
/// `mana_abilities::resolve_mana_ability`. The inline mana-ability resolver now
/// always routes through `resolve_mana_types_for_ability` so the cost-paid
/// object snapshot (Food Chain class) and `chosen_x` are visible. Kept as a
/// minimal building block for callers that have neither.
#[allow(dead_code)]
pub(crate) fn resolve_mana_types(
    produced: &ManaProduction,
    state: &GameState,
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<ManaType> {
    resolve_mana_types_impl(produced, state, None, None, controller, source_id)
}

/// Variant of `resolve_mana_types` that threads the resolving ability's context
/// (including `chosen_x`) into quantity resolution. Use this from stack-resolving
/// effect handlers (`effects::mana::resolve`).
fn resolve_mana_types_with_ability(
    produced: &ManaProduction,
    state: &GameState,
    ability: &ResolvedAbility,
) -> Vec<ManaType> {
    resolve_mana_types_impl(
        produced,
        state,
        Some(ability),
        Some(ability),
        ability.controller,
        ability.source_id,
    )
}

/// CR 117.1 + CR 202.3: Public-crate wrapper for `resolve_mana_types_with_ability`.
/// Used by the inline mana-ability resolver in `mana_abilities.rs` to thread a
/// `ResolvedAbility` carrying `cost_paid_object` (Food Chain class)
/// and `chosen_x` into the production-count resolution.
pub(crate) fn resolve_mana_types_for_ability(
    produced: &ManaProduction,
    state: &GameState,
    ability: &ResolvedAbility,
) -> Vec<ManaType> {
    resolve_mana_types_with_ability(produced, state, ability)
}

pub(crate) fn resolve_mana_types_with_contexts(
    produced: &ManaProduction,
    state: &GameState,
    color_ability: Option<&ResolvedAbility>,
    count_ability: &ResolvedAbility,
) -> Vec<ManaType> {
    resolve_mana_types_impl(
        produced,
        state,
        Some(count_ability),
        color_ability,
        count_ability.controller,
        count_ability.source_id,
    )
}

fn resolve_count(
    count: &crate::types::ability::QuantityExpr,
    state: &GameState,
    ability: Option<&ResolvedAbility>,
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
) -> usize {
    let raw = match ability {
        Some(a) => resolve_quantity_with_targets(state, count, a),
        None => resolve_quantity(state, count, controller, source_id),
    };
    raw.max(0) as usize
}

fn resolve_mana_types_impl(
    produced: &ManaProduction,
    state: &GameState,
    ability: Option<&ResolvedAbility>,
    color_ability: Option<&ResolvedAbility>,
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<ManaType> {
    // A source-only survey has no invocation authority. Absence of a viable
    // output is the existing unavailable result here, before scalar evaluation.
    if ability.is_none() {
        let mut addressed = false;
        produced.for_each_quantity_expr(&mut |quantity| {
            quantity.for_each_ref(&mut |reference| {
                addressed |= reference.instruction_result_id().is_some();
            })
        });
        if addressed {
            return Vec::new();
        }
    }
    match produced {
        // CR 106.1a: Colored mana is produced in the five standard colors.
        ManaProduction::Fixed { colors, .. } => colors.iter().map(mana_color_to_type).collect(),
        // CR 106.1b: Colorless mana is a type of mana distinct from colored mana.
        ManaProduction::Colorless { count } => {
            vec![ManaType::Colorless; resolve_count(count, state, ability, controller, source_id)]
        }
        // CR 106.5: If an ability would produce one or more mana of an undefined type,
        // it produces no mana instead.
        ManaProduction::AnyOneColor { color_options, .. } => {
            let Some(&color) = color_options.first() else {
                return Vec::new();
            };
            // CR 608.2d + CR 700.5: The count reads this production's selected
            // color, including when the default path makes that choice.
            let bound = bind_any_one_color_choice(produced, color);
            let ManaProduction::AnyOneColor { count, .. } = &*bound else {
                unreachable!("binding preserves the production variant");
            };
            let amount = resolve_count(count, state, ability, controller, source_id);
            vec![mana_color_to_type(&color); amount]
        }
        ManaProduction::AnyCombination {
            count,
            color_options,
        } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            if color_options.is_empty() {
                return Vec::new();
            }
            (0..amount)
                .map(|index| mana_color_to_type(&color_options[index % color_options.len()]))
                .collect()
        }
        ManaProduction::ChosenColor {
            count,
            fixed_alternative,
            ..
        } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            match (chosen_color_for_mana(state, source_id), fixed_alternative) {
                // A color was chosen — produce that color.
                (Some(color), _) => vec![mana_color_to_type(&color); amount],
                // CR 106.1: count derivation must be independent of color
                // resolvability — the SingleColor choice supplies the actual
                // color. When a fixed alternative exists, the no-prompt default
                // path (auto-tap / AI direct activation) produces the fixed
                // color deterministically; the count-derivation path
                // (`chosen_mana_types_for_prompt`) overrides the color with the
                // player's `SingleColor` choice, so the length is what matters.
                (None, Some(fixed)) => vec![mana_color_to_type(fixed); amount],
                // CR 106.5: pure chosen-color production with no color chosen
                // produces no mana (undefined type).
                (None, None) => Vec::new(),
            }
        }
        // CR 106.1b + CR 106.5: Jeweled Amulet — "Add one mana of this
        // artifact's last noted type." Unlike `ChosenColor` (a
        // player-prompted `ManaColor`), the noted value is engine-set
        // (`Effect::NoteManaSpent`) and `ManaType`-valued (colorless is a
        // real noted type per the card's ruling). A card in this class always
        // notes exactly one type — its cost's own generic mana is spent as a
        // single unit-worth of one type — so, mirroring `AnyOneColor`'s
        // repeat-by-count idiom, the first noted type repeats `count` times.
        // No noted type (never activated the noting ability, or a fresh
        // incarnation after a zone change) produces no mana.
        ManaProduction::NotedType { count } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            match noted_mana_type_for(state, source_id) {
                Some(mana_type) => vec![mana_type; amount],
                None => Vec::new(),
            }
        }
        // CR 106.7: Produce mana of any color that a land an opponent controls could produce.
        // Delegates to mana_sources::opponent_land_color_options for the shared computation.
        ManaProduction::OpponentLandColors { count } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let color_options = mana_sources::opponent_land_color_options(state, controller);
            // CR 106.5 + CR 106.7: If no color can be defined (including when the
            // surveyed lands could produce only {C}, which is not a color per
            // CR 106.1a), produce no mana.
            let Some(first) = color_options.first().map(mana_color_to_type) else {
                return Vec::new();
            };
            vec![first; amount]
        }
        // CR 106.1 + CR 106.5 + CR 202.2c: Omnath, Locus of All — "add three mana
        // in any combination of its colors." The color set is the scoped object's
        // colors (dynamic, mirroring AnyOneColorAmongPermanents), not a static
        // option list. This is the no-override default path; the per-unit free
        // choice is surfaced by `mana_choice_prompt` (ManaChoicePrompt::AnyCombination)
        // when the object has more than one color. Without an override the colors
        // are cycled, mirroring the static AnyCombination default. CR 106.5: a
        // colorless / unbound object produces no mana.
        ManaProduction::AnyCombinationOfObjectColors { count, scope } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let color_options = object_colors_for_scope(state, ability, *scope);
            if color_options.is_empty() {
                return Vec::new();
            }
            (0..amount)
                .map(|index| mana_color_to_type(&color_options[index % color_options.len()]))
                .collect()
        }
        // CR 106.7 + CR 106.1b: Reflecting Pool class — produce N mana of any
        // type (W/U/B/R/G/C) that a land matching `land_filter` could produce.
        // Without an explicit choice override (auto-tap during cost payment, or
        // direct activation without prompt), the first listed type is produced
        // mirroring the `OpponentLandColors` / `AnyOneColor` precedent. The
        // per-type choice prompt is surfaced by `mana_choice_prompt` when the
        // option set has more than one type. CR 106.5: an empty option set
        // (no matching lands, or only mutually-recursive producers) produces
        // no mana.
        ManaProduction::AnyTypeProduceableBy { count, land_filter } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let type_options = mana_sources::produceable_mana_types_by_filter(
                state,
                land_filter,
                controller,
                source_id,
                color_ability,
            );
            let Some(first) = type_options.first().copied() else {
                return Vec::new();
            };
            vec![first; amount]
        }
        // CR 605.1a + CR 406.1 + CR 610.3: One mana of any of the colors among the
        // cards exiled-with this source (Pit of Offerings). Reads `state.exile_links`
        // for the relation; the per-color choice is selected by the caller via
        // `color_override` (auto-tap during cost payment, or AI/UI on direct activation),
        // exactly like `AnyOneColor`. Without an override the first listed color is
        // produced. CR 106.5: undefined mana type → produce no mana.
        ManaProduction::ChoiceAmongExiledColors { source } => {
            let color_options = exiled_color_options(state, *source, source_id);
            let Some(first) = color_options.first().copied() else {
                return Vec::new();
            };
            vec![first]
        }
        // CR 605.3b + CR 106.1a: Filter-land combinations. When no override is
        // supplied (stack-resolving paths or direct activation without choice),
        // fall back to the first listed combination — mirrors the
        // `ChoiceAmongExiledColors` precedent. `produce_mana_from_ability`
        // selects the combination via `ProductionOverride::Combination`, so
        // this branch is only hit on the "no override at all" path.
        ManaProduction::ChoiceAmongCombinations { options } => options
            .first()
            .map(|combo| combo.iter().map(mana_color_to_type).collect())
            .unwrap_or_default(),
        // CR 106.1: Mixed colorless + colored production (e.g. {C}{W}, {C}{C}{R}).
        ManaProduction::Mixed {
            colorless_count,
            colors,
        } => {
            let mut mana = vec![ManaType::Colorless; *colorless_count as usize];
            mana.extend(colors.iter().map(mana_color_to_type));
            mana
        }
        // CR 903.4 + CR 903.4f + CR 106.5: Produce mana of one color from the
        // activator's commander color identity. Without a color_override
        // (auto-tap, or no choice needed) this picks the first listed color,
        // mirroring the `ChoiceAmongExiledColors` / `AnyOneColor` precedent.
        // The color-choice prompt is driven by `mana_choice_prompt` when
        // identity.len() > 1. If the identity is empty — no commander or an
        // undefined identity per CR 903.4f — the ability produces no mana.
        ManaProduction::AnyInCommandersColorIdentity { count, .. } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let identity = super::super::commander::commander_color_identity(state, controller);
            let Some(first) = identity.first() else {
                return Vec::new();
            };
            vec![mana_color_to_type(first); amount]
        }
        // CR 106.1 + CR 109.1: Produce one mana of each distinct color (W/U/B/R/G)
        // found among permanents matching `filter`. Used by Faeburrow Elder.
        // Returns empty when no colored permanent matches (CR 106.5).
        ManaProduction::DistinctColorsAmongPermanents { filter } => {
            distinct_colors_among_permanents(state, color_ability, source_id, filter)
                .into_iter()
                .map(|c| mana_color_to_type(&c))
                .collect()
        }
        // CR 106.1 + CR 109.1: Mox Amber — one chosen color from among matching
        // permanents. Without a color_override, produce the first listed color
        // (mirrors ChoiceAmongExiledColors / AnyOneColor). CR 106.5: empty set
        // → no mana.
        ManaProduction::AnyOneColorAmongPermanents { count, filter, .. } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let color_options =
                distinct_colors_among_permanents(state, color_ability, source_id, filter);
            let Some(first) = color_options.first().copied() else {
                return Vec::new();
            };
            vec![mana_color_to_type(&first); amount]
        }
        // CR 603.7c + CR 106.3 + CR 106.5 + CR 106.12a: Vorinclex / Dictate of
        // Karametra — "add one mana of any type that land produced." The set of
        // produced types is read from the triggering `TappedForMana` event
        // carried in `state.current_trigger_event` at resolution time. The
        // `TapsForMana` trigger fires once per mana-ability resolution
        // (CR 106.12a), so this branch sees the full produced set, not a single
        // unit. If the current event is absent (off-stack resolution) or not a
        // `TappedForMana` event, this produces no mana (CR 106.5 — undefined
        // mana type).
        //
        // This branch only runs when no choice is owed: a multi-type event
        // pauses earlier in the echo pre-check in `resolve` (CR 608.2d
        // SingleColor choice, completed for exactly one mana), in both the
        // inline fixed-point path and the stack path; the Pass-1 skip keeps
        // the bare loop out, and single-type echo falls through here.
        // Emitting one unit per *distinct* color here therefore always yields
        // exactly one mana.
        ManaProduction::TriggerEventManaType => {
            use crate::types::events::GameEvent;
            match &state.current_trigger_event {
                Some(
                    GameEvent::TappedForMana { produced, .. }
                    | GameEvent::ManaAbilityProduced { produced, .. },
                ) => {
                    let distinct: std::collections::HashSet<_> = produced.iter().copied().collect();
                    distinct.into_iter().collect()
                }
                _ => Vec::new(),
            }
        }
    }
}

/// CR 106.1 + CR 109.1: Shared helper returning the distinct colors (W/U/B/R/G)
/// present among permanents matching `filter`. Colorless permanents contribute
/// nothing. Used by both the mana ability resolver and `mana_sources` so that
/// cost-payment and direct activation see the same option set.
/// CR 202.2c + CR 106.5: Colors of the object identified by `scope`, for an
/// `AnyCombinationOfObjectColors` mana production (Omnath, Locus of All). Reads
/// the object's current `color` (zone-independent — correct after the revealed
/// card is put into hand, per CR 400.7j), returned in stable WUBRG order. Empty
/// when the scope binds no object or the object is colorless (CR 106.5). Only
/// `ObjectScope::Target` has a printing today; other scopes bind no object.
pub(crate) fn object_colors_for_scope(
    state: &GameState,
    ability: Option<&ResolvedAbility>,
    scope: ObjectScope,
) -> Vec<ManaColor> {
    let obj_id = match scope {
        ObjectScope::Target => ability.and_then(|a| {
            a.targets.iter().find_map(|t| match t {
                TargetRef::Object(id) => Some(*id),
                _ => None,
            })
        }),
        _ => None,
    };
    let Some(obj) = obj_id.and_then(|id| state.objects.get(&id)) else {
        return Vec::new();
    };
    [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ]
    .into_iter()
    .filter(|c| obj.color.contains(c))
    .collect()
}

pub(crate) fn distinct_colors_among_permanents(
    state: &GameState,
    ability: Option<&ResolvedAbility>,
    source_id: crate::types::identifiers::ObjectId,
    filter: &crate::types::ability::TargetFilter,
) -> Vec<crate::types::mana::ManaColor> {
    use crate::game::filter::{matches_target_filter, FilterContext};
    let mut available = true;
    filter.for_each_nested_quantity_ref(&mut |reference| {
        available &= crate::game::quantity::addressed_quantity_ref_is_available(
            state,
            reference,
            ability.and_then(|ability| ability.return_result_process),
        );
    });
    if !available {
        return Vec::new();
    }
    let filter_ctx = match ability {
        Some(a) => FilterContext::from_ability(a),
        None => FilterContext::from_source(state, source_id),
    };
    let zone = filter
        .extract_in_zone()
        .unwrap_or(crate::types::zones::Zone::Battlefield);
    let mut seen: std::collections::HashSet<crate::types::mana::ManaColor> =
        std::collections::HashSet::new();
    for &id in crate::game::targeting::zone_object_ids(state, zone).iter() {
        if !matches_target_filter(state, id, filter, &filter_ctx) {
            continue;
        }
        if let Some(obj) = state.objects.get(&id) {
            for color in &obj.color {
                seen.insert(*color);
            }
        }
    }
    // Stable order for determinism (WUBRG).
    use crate::types::mana::ManaColor;
    [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ]
    .into_iter()
    .filter(|c| seen.contains(c))
    .collect()
}

/// CR 605.1a + CR 406.1 + CR 610.3: Resolve the legal `ManaType` set for a
/// `ChoiceAmongExiledColors` mana ability. Reads `state.exile_links` keyed to the
/// scope, collects the printed colors of every still-exiled linked object, and
/// drops colorless cards (CR 106.5). Shared by the resolver here and by
/// `mana_sources::mana_options_from_production` so cost-payment and direct
/// activation see the same legal set.
pub(crate) fn exiled_color_options(
    state: &GameState,
    scope: LinkedExileScope,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<ManaType> {
    let mut options: Vec<ManaType> = Vec::new();
    // The object comes back WITH the id: `linked_exiled_ids` already resolved it through
    // `state.objects.get(&link.exiled_id)?` and drops every id it cannot resolve, so a
    // second lookup here would have a provably unreachable `else` arm.
    for (_, exiled) in linked_exiled_ids(state, scope, source_id) {
        // CR 202.3d + CR 709.4b: a linked exiled card is off the stack, so a split
        // card exposes the combined colors of both halves, not just its front half.
        for color in exiled.effective_colors() {
            let mana_type = mana_color_to_type(&color);
            if !options.contains(&mana_type) {
                options.push(mana_type);
            }
        }
    }
    options
}

/// CR 607.2a: the LINK RELATION an exiled-colour mana ability reads — "the second ability
/// refers only to cards in the exile zone that were put there as a result of an instruction to
/// exile them in the first ability". Yields, in `state.exile_links` order, the ids linked to
/// `source_id` under `scope` that are STILL in exile, EACH WITH THE OBJECT IT RESOLVED TO. The
/// object is not a convenience: deciding the `zone == Exile` conjunct already resolves
/// `state.objects.get(&link.exiled_id)`, so every yielded id provably HAS a live entry and no
/// consumer needs an `else` arm that can never be taken.
///
/// The single link authority for [`exiled_color_options`].
///
/// ORDER IS PART OF THE CONTRACT: link order, not a set, because [`exiled_color_options`] returns
/// its options in it. The guards are that function's `#[cfg(test)]` assertions
/// (`exiled_color_options_use_combined_split_colors`, `pit_of_offerings_*` in `mana_abilities.rs`).
pub(crate) fn linked_exiled_ids(
    state: &GameState,
    scope: LinkedExileScope,
    source_id: crate::types::identifiers::ObjectId,
) -> impl Iterator<
    Item = (
        crate::types::identifiers::ObjectId,
        &crate::game::game_object::GameObject,
    ),
> + '_ {
    let host_id = match scope {
        LinkedExileScope::ThisObject => source_id,
    };
    state.exile_links.iter().filter_map(move |link| {
        if link.source_id != host_id {
            return None;
        }
        let exiled = state.objects.get(&link.exiled_id)?;
        // CR 400.7: Only consider linked cards still in exile (links are pruned
        // from `state.exile_links` when the exiled card leaves exile, but guard
        // defensively in case ordering interleaves).
        if exiled.zone != crate::types::zones::Zone::Exile {
            return None;
        }
        Some((link.exiled_id, exiled))
    })
}

pub(crate) fn chosen_color_for_mana(
    state: &GameState,
    source_id: crate::types::identifiers::ObjectId,
) -> Option<ManaColor> {
    state
        .objects
        .get(&source_id)
        .and_then(|obj| obj.chosen_color())
        .or_else(|| {
            state
                .last_named_choice
                .as_ref()
                .and_then(|choice| match choice {
                    ChoiceValue::Color(color) => Some(*color),
                    _ => None,
                })
        })
}

/// CR 700.5 + CR 608.2d: Whether an any-one-color production's amount reads
/// the color chosen for it ("equal to your devotion to that color": Nykthos,
/// Nyx Lotus, the plane Nyx). Within `AnyOneColor` the only choice such a
/// count can name is the production's own, since the parser emits it only for
/// "Choose a color. Add ... of that color".
pub(crate) fn any_one_color_count_reads_choice(produced: &ManaProduction) -> bool {
    let ManaProduction::AnyOneColor { count, .. } = produced else {
        return false;
    };
    count.any_ref(&mut |qty| {
        matches!(
            qty,
            QuantityRef::Devotion {
                colors: DevotionColors::ChosenColor
            }
        )
    })
}

/// CR 106.1 + CR 605.3b + CR 608.2d: `produced` with its amount bound to
/// `color`, the color chosen for this production as it resolves. A count that
/// reads "that color" reads `color` itself, never a color stored on the source
/// or left over from another choice; nothing in the game state is written.
/// Other productions are returned unchanged.
pub(crate) fn bind_any_one_color_choice(
    produced: &ManaProduction,
    color: ManaColor,
) -> std::borrow::Cow<'_, ManaProduction> {
    if !any_one_color_count_reads_choice(produced) {
        return std::borrow::Cow::Borrowed(produced);
    }
    let mut bound = produced.clone();
    if let ManaProduction::AnyOneColor { count, .. } = &mut bound {
        count.each_ref_mut(&mut |qty| {
            if let QuantityRef::Devotion { colors } = qty {
                if *colors == DevotionColors::ChosenColor {
                    *colors = DevotionColors::Fixed(vec![color]);
                }
            }
        });
    }
    std::borrow::Cow::Owned(bound)
}

/// CR 106.1 + CR 608.2d: The amount an any-one-color production adds for each
/// color it could be activated for, when that amount depends on the color
/// (see [`any_one_color_count_reads_choice`]); empty otherwise. Colors that
/// would add no mana are omitted, and the rest are best first (ties keep the
/// printed order), so a planner taking one row for generic mana takes the
/// color that adds the most.
pub(crate) fn any_one_color_outputs(
    state: &GameState,
    produced: &ManaProduction,
    ability: &ResolvedAbility,
) -> Vec<(ManaType, usize)> {
    let ManaProduction::AnyOneColor { color_options, .. } = produced else {
        return Vec::new();
    };
    if !any_one_color_count_reads_choice(produced) {
        return Vec::new();
    }
    let mut outputs: Vec<(ManaType, usize)> = color_options
        .iter()
        .map(|color| {
            (
                mana_color_to_type(color),
                any_one_color_amount(produced, state, ability, *color),
            )
        })
        .filter(|(_, amount)| *amount > 0)
        .collect();
    outputs.sort_by_key(|(_, amount)| std::cmp::Reverse(*amount));
    outputs
}

/// CR 106.1 + CR 608.2d: The amount an any-one-color production adds when
/// `color` is chosen for it, as a scalar: the count is bound to `color`
/// ([`bind_any_one_color_choice`]) and resolved directly, with no mana units
/// built. The scalar authority for planning; resolution builds its units from
/// the same bound count.
fn any_one_color_amount(
    produced: &ManaProduction,
    state: &GameState,
    ability: &ResolvedAbility,
    color: ManaColor,
) -> usize {
    let bound = bind_any_one_color_choice(produced, color);
    match &*bound {
        ManaProduction::AnyOneColor {
            count,
            color_options,
            ..
        } if !color_options.is_empty() => resolve_count(
            count,
            state,
            Some(ability),
            ability.controller,
            ability.source_id,
        ),
        _ => 0,
    }
}

/// CR 106.1b: The first mana type noted by a past `Effect::NoteManaSpent`
/// resolution on `source_id` ("this artifact's last noted type" — Jeweled
/// Amulet). Unlike `chosen_color_for_mana`, this is never player-prompted —
/// engine-set state only, with no `last_named_choice` fallback.
pub(crate) fn noted_mana_type_for(
    state: &GameState,
    source_id: crate::types::identifiers::ObjectId,
) -> Option<ManaType> {
    state
        .objects
        .get(&source_id)
        .and_then(|obj| obj.noted_mana_spent())
        .and_then(|types| types.first().copied())
}

/// Convert a ManaColor to the runtime ManaType.
/// CR 106.1a: There are five colors of mana: white, blue, black, red, and green.
/// CR 106.1b: There are six types of mana: white, blue, black, red, green, and colorless.
fn mana_color_to_type(color: &ManaColor) -> ManaType {
    match color {
        ManaColor::White => ManaType::White,
        ManaColor::Blue => ManaType::Blue,
        ManaColor::Black => ManaType::Black,
        ManaColor::Red => ManaType::Red,
        ManaColor::Green => ManaType::Green,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ChoiceValue, ControllerRef, DevotionColors,
        QuantityExpr, QuantityRef, TargetFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaCost, ManaUnit};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_mana_ability(produced: ManaProduction) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Mana {
                produced,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    /// CR 202.3d + CR 709.4b: "add mana of any of the exiled card's colors"
    /// (`ChoiceAmongExiledColors` → `exiled_color_options`) reads a linked exiled
    /// split card's COMBINED colors. Assault // Battery is {R} (front, Red) +
    /// {3}{G} (Green) → colors {Red, Green} off the stack.
    ///
    /// Revert-failing discriminator: reading the front-only `exiled.color` (Red)
    /// omits Green from the options, so the `contains(Green)` assertion fails.
    #[test]
    fn exiled_color_options_use_combined_split_colors() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::game::scenario_db::GameScenarioDbExt;
        use crate::types::game_state::{ExileLink, ExileLinkKind};

        let db = crate::test_support::shared_card_db();
        let mut sc = GameScenario::new();
        let source = sc.add_real_card(P0, "Gray Ogre", Zone::Battlefield, db);
        let exiled = sc.add_real_card(P0, "Assault", Zone::Exile, db);
        let mut state = sc.state;
        state.exile_links.push(ExileLink {
            exiled_id: exiled,
            source_id: source,
            kind: ExileLinkKind::TrackedBySource,
        });

        let options = exiled_color_options(&state, LinkedExileScope::ThisObject, source);
        assert!(
            options.contains(&ManaType::Red) && options.contains(&ManaType::Green),
            "a linked exiled Assault // Battery must expose BOTH Red and Green (its \
             combined split colors); the front-only read would omit Green — got {options:?}"
        );
    }

    /// `linked_exiled_ids` yields link-relation ORDER, and `exiled_color_options` preserves it
    /// into the offered option vector.
    ///
    /// The two pre-existing guards structurally cannot measure this:
    /// `exiled_color_options_use_combined_split_colors` has a SINGLE link, and the
    /// `pit_of_offerings_*` guards have three links but only one COLORED card — under either the
    /// produced vector has one element and every ordering agrees. This board is the smallest one
    /// on which orderings disagree: two links, two DIFFERENT colors. The claim is a positional
    /// `assert_eq!` on the whole vector, deliberately not a `contains` pair, a set comparison, or
    /// a sorted compare — each of those is order-blind and would restate the gap, not close it.
    ///
    /// REVERT / MUTATION PROBE: change `linked_exiled_ids`' `state.exile_links.iter()` to
    /// `.iter().rev()` ⇒ **FAILS** on the link-order assertion.
    #[test]
    fn linked_exiled_ids_preserves_link_order_into_the_offered_colors() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::game::scenario_db::GameScenarioDbExt;
        use crate::types::game_state::{ExileLink, ExileLinkKind};

        let db = crate::test_support::shared_card_db();
        let mut sc = GameScenario::new();
        let source = sc.add_real_card(P0, "Gray Ogre", Zone::Battlefield, db);
        // Linked FIRST: mono-GREEN. Linked SECOND: mono-RED. Real cards, so a card-data
        // colour change fails a reach-guard below rather than silently re-pointing the
        // order claim.
        let green = sc.add_real_card(P0, "Grizzly Bears", Zone::Exile, db);
        let red = sc.add_real_card(P0, "Gray Ogre", Zone::Exile, db);
        let mut state = sc.state;
        for exiled_id in [green, red] {
            state.exile_links.push(ExileLink {
                exiled_id,
                source_id: source,
                kind: ExileLinkKind::TrackedBySource,
            });
        }

        // ── REACH-GUARDS, before the order assertion ─────────────────────────────────
        // Deliberately ORDER-INSENSITIVE. This guard's job is to prove BOTH links
        // survive the CR 607.2a source and CR 400.7 zone conjuncts, so the option vector
        // really has two elements and orderings can disagree. Asserting order here too
        // would shadow the order assertion below and steal the mutation that proves it.
        let survivors = linked_exiled_ids(&state, LinkedExileScope::ThisObject, source)
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        assert_eq!(
            survivors.len(),
            2,
            "reach-guard: BOTH links must survive. If either were filtered out the option \
             vector would have ONE element, every ordering would agree, and the order \
             assertion below would be vacuous — got {survivors:?}"
        );
        assert!(
            survivors.contains(&green) && survivors.contains(&red),
            "reach-guard: the survivors are exactly the two cards linked above — \
             got {survivors:?}"
        );
        assert_eq!(
            state.objects[&green]
                .effective_colors()
                .into_iter()
                .map(|c| mana_color_to_type(&c))
                .collect::<Vec<_>>(),
            vec![ManaType::Green],
            "reach-guard: the first-linked card must be mono-GREEN — the order claim is \
             only observable because the two links contribute DIFFERENT colours"
        );
        assert_eq!(
            state.objects[&red]
                .effective_colors()
                .into_iter()
                .map(|c| mana_color_to_type(&c))
                .collect::<Vec<_>>(),
            vec![ManaType::Red],
            "reach-guard: the second-linked card must be mono-RED"
        );

        let options = exiled_color_options(&state, LinkedExileScope::ThisObject, source);
        assert_eq!(
            options,
            vec![ManaType::Green, ManaType::Red],
            "MED-3: `exiled_color_options` must offer the colours in LINK-RELATION order — \
             Green (linked first) then Red (linked second). This is the ORDER half of the \
             C2 extraction's identity contract, and it is why `linked_exiled_ids` yields \
             ids in `state.exile_links` order rather than collecting a set. Changing that \
             `.iter()` to `.iter().rev()` makes this FAIL with `[Red, Green]`"
        );
    }

    #[test]
    fn produce_single_red_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    /// CR 106.4: for "Choose a player. That player adds one mana of any color
    /// they choose" (Spectral Searchlight, Stadium Vendors) the CHOSEN player —
    /// not the controller — is both prompted to pick the color and receives the
    /// mana. Driven through the production `resolve` path (which publishes the
    /// color prompt) into the `handle_choose_mana_effect` completion, so both the
    /// prompted player and the deposit are asserted. Revert-probe: without the
    /// recipient derivation the prompt is directed to P0 and the mana lands in
    /// P0's pool.
    #[test]
    fn chosen_player_mana_prompt_and_deposit_go_to_the_recipient() {
        let mut state = GameState::new_two_player(42);
        // The mana source on the battlefield, controlled by P0.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Spectral Searchlight".to_string(),
            Zone::Battlefield,
        );

        // P0 controls the effect and chose opponent P1 as the recipient.
        let mut ability = ResolvedAbility::new(
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
                target: Some(ManaTargetRole::Recipient {
                    recipient: TargetFilter::ScopedPlayer,
                }),
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.scoped_player = Some(PlayerId(1));

        // Production path: `resolve` publishes the color prompt to the CHOSEN player.
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let (prompt, ctx_ability) = match &state.waiting_for {
            WaitingFor::ChooseManaColor {
                player,
                choice,
                context,
                ..
            } => {
                assert_eq!(
                    *player,
                    PlayerId(1),
                    "the chosen player (not the controller) must be prompted to pick the color"
                );
                let ctx = match context {
                    ManaChoiceContext::ResolvingEffect(a) => (**a).clone(),
                    other => panic!("expected ResolvingEffect context, got {other:?}"),
                };
                (choice.clone(), ctx)
            }
            other => panic!("expected a ChooseManaColor prompt, got {other:?}"),
        };

        // Completion: P1 picks Blue; the mana lands in P1's pool, not P0's.
        let mut events2 = Vec::new();
        handle_choose_mana_effect(
            &mut state,
            &ctx_ability,
            PlayerId(1),
            &prompt,
            None,
            ManaChoice::SingleColor(ManaType::Blue),
            &mut events2,
        )
        .unwrap();
        assert_eq!(
            state.players[1].mana_pool.count_color(ManaType::Blue),
            1,
            "chosen recipient (P1) must receive the mana"
        );
        assert_eq!(state.players[1].mana_pool.total(), 1);
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "controller (P0) must NOT receive the chosen player's mana"
        );
    }

    /// CR 115.1 + CR 106.4: "Target player adds that much {C}" (Jetfire) — a
    /// genuine `TargetFilter::Player` recipient whose count is NOT target-derived
    /// deposits into the chosen target player (`ability.targets`), not the
    /// controller. Revert-probe: before the `TargetFilter::Player` arm in
    /// `mana_effect_recipient`, the mana lands in P0's pool.
    #[test]
    fn target_player_recipient_deposits_into_the_target_not_controller() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jetfire".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 3 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: Some(ManaTargetRole::Recipient {
                    recipient: TargetFilter::Player,
                }),
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[1].mana_pool.count_color(ManaType::Colorless),
            3,
            "the targeted player (P1) must receive the mana"
        );
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "controller (P0) must NOT receive a targeted recipient's mana"
        );
    }

    /// Matrix row 5 (runtime half) — a CONTEXT-REF recipient plus a REAL count
    /// source. The recipient occupies NO surfaced slot, so the count source
    /// lands at surfaced index 0; naive "recipient == targets[0]" index math
    /// breaks exactly here.
    ///
    /// This is the subject-predicate shape ("That player adds {R} for each card
    /// in target opponent's hand", Blinkmoth Urn's route). CR 106.4: the mana
    /// goes to the scoped player. CR 115.1: the amount is read from the chosen
    /// count-source player's hand — NOT the scoped player's, and NOT the
    /// controller's.
    #[test]
    fn context_ref_recipient_with_real_count_source_reads_the_count_slot() {
        use crate::types::ability::{ControllerRef, ManaProduction, ManaTargetRole, ZoneRef};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Subject Led Count Source".to_string(),
            Zone::Battlefield,
        );

        // P1 is BOTH the scoped recipient and the chosen count source here only
        // by construction of the two-player fixture; the discriminating fact is
        // that the count must come from the TARGET slot, so give P1 a hand of a
        // size the controller does not share.
        for i in 0..4 {
            create_object(
                &mut state,
                CardId(50 + i),
                PlayerId(1),
                format!("Count Card {i}"),
                Zone::Hand,
            );
        }

        let mut ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::TargetZoneCardCount {
                            zone: ZoneRef::Hand,
                            scope: ControllerRef::TargetOpponent,
                            binding: crate::types::ability::CountBinding::Explicit,
                        },
                    },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: Some(ManaTargetRole::Both {
                    recipient: TargetFilter::ScopedPlayer,
                    count_source: TargetFilter::Player,
                }),
            },
            // ONE target: the count source, at surfaced index 0. The context-ref
            // recipient surfaces no slot and so consumes no target.
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );
        ability.scoped_player = Some(PlayerId(1));

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("subject-led count-source mana resolves");

        assert_eq!(
            state.players[1].mana_pool.count_color(ManaType::Colorless),
            4,
            "CR 106.4 + CR 115.1: the scoped recipient receives mana equal to the \
             COUNT SOURCE slot's hand size"
        );
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "the controller receives nothing"
        );
    }

    /// Matrix row 10a — CR 608.2b clause (1): an illegal RECIPIENT means "that
    /// part of the effect does nothing", and the mana is NOT quietly redirected
    /// to the controller (which would hand a targeted opponent's mana to the
    /// caster). The count source's half is unaffected, and positions are stable.
    ///
    /// The chosen recipient here is the CONTROLLER while the recipient filter
    /// demands an OPPONENT, so the target is illegal at resolution. A
    /// `mana_effect_recipient` that skipped per-slot re-validation would find
    /// P0 and deposit into it — which is exactly what this asserts must not
    /// happen. Reach guard: the paired legal case below deposits a non-zero
    /// amount through the same code path.
    #[test]
    fn illegal_recipient_target_deposits_no_mana_anywhere() {
        use crate::types::ability::{ControllerRef, ManaProduction, ManaTargetRole, TypedFilter};

        let opponent_only =
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));

        let build = |chosen: PlayerId| {
            ResolvedAbility::new(
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 3 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: Some(ManaTargetRole::Recipient {
                        recipient: opponent_only.clone(),
                    }),
                },
                vec![TargetRef::Player(chosen)],
                ObjectId(100),
                PlayerId(0),
            )
        };

        // Reach guard / positive: a LEGAL opponent recipient does receive mana,
        // proving the negative below is not a vacuous "nothing ever resolves".
        let mut state = GameState::new_two_player(42);
        let legal = build(PlayerId(1));
        let mut events = Vec::new();
        crate::game::effects::mana::resolve(&mut state, &legal, &mut events)
            .expect("a legal opponent recipient resolves");
        assert_eq!(
            state.players[1].mana_pool.total(),
            3,
            "reach guard: the legal recipient must actually receive the mana"
        );
        assert_eq!(state.players[0].mana_pool.total(), 0);

        // Negative: the CONTROLLER is not a legal "target opponent".
        let mut state = GameState::new_two_player(42);
        let illegal = build(PlayerId(0));
        let mut events = Vec::new();
        crate::game::effects::mana::resolve(&mut state, &illegal, &mut events)
            .expect("an illegal recipient still resolves the effect, adding nothing");
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "CR 608.2b: an illegal recipient receives nothing and the mana is NOT \
             redirected to the controller"
        );
        assert_eq!(state.players[1].mana_pool.total(), 0);
    }

    /// CR 601.2c + CR 106.4: multi-authority provenance fixture for the two mana
    /// roles. The identical `TargetFilter::Player` is emitted both as a RECIPIENT
    /// (subject-led "target player adds", count target-independent) and as a
    /// COUNT SOURCE ("Add {U} for each card in target player's hand", count =
    /// `TargetZoneCardCount`). The ROLE now STATES which is which — the previous
    /// `mana_count_reads_targets` quantity-shape inference that had to GUESS is
    /// deleted, so its two assertions are deliberately NOT ported. Only the
    /// recipient role redirects the pool; a count-source-only role (and Jeska's
    /// Will's `Typed(Opponent)`) leaves the recipient on the controller. Tested at
    /// the `mana_effect_recipient` seam to avoid coupling to count resolution;
    /// the end-to-end runtime counterparts are
    /// `target_player_recipient_deposits_into_the_target_not_controller` and the
    /// `mana_target_recipient_and_count_source` integration test.
    #[test]
    fn mana_role_separates_recipient_from_count_source() {
        use crate::types::ability::{ControllerRef, TypedFilter, ZoneRef};

        let state = GameState::new_two_player(42);
        let src = ObjectId(100);
        let targets = vec![TargetRef::Player(PlayerId(1))];

        // Count-source production: the count reads a player target.
        let count_source = ManaProduction::AnyOneColor {
            count: QuantityExpr::Ref {
                qty: QuantityRef::TargetZoneCardCount {
                    zone: ZoneRef::Hand,
                    scope: ControllerRef::TargetPlayer,
                    binding: crate::types::ability::CountBinding::Explicit,
                },
            },
            color_options: vec![ManaColor::Blue],
            contribution: ManaContribution::Base,
        };

        // Recipient production: a target-independent count ("that much" →
        // EventContextAmount).
        let recipient_prod = ManaProduction::Colorless {
            count: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
        };

        let mk = |produced: ManaProduction, role: ManaTargetRole| {
            ResolvedAbility::new(
                Effect::Mana {
                    produced,
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: Some(role),
                },
                targets.clone(),
                src,
                PlayerId(0),
            )
        };

        // (a) Recipient role, target-independent count → the target (P1).
        let recipient_role = ManaTargetRole::Recipient {
            recipient: TargetFilter::Player,
        };
        let recip = mk(recipient_prod.clone(), recipient_role.clone());
        assert_eq!(
            mana_effect_recipient(&state, &recip, Some(&recipient_role)),
            Some(PlayerId(1)),
            "a Recipient role resolves to its own chosen target"
        );

        // (b) The SAME `TargetFilter::Player`, now declared as a COUNT SOURCE →
        // the controller (P0). This is the case the deleted quantity-shape
        // inference used to guess; the role states it.
        let count_role = ManaTargetRole::CountSource {
            count_source: TargetFilter::Player,
        };
        let cs = mk(count_source.clone(), count_role.clone());
        assert_eq!(
            mana_effect_recipient(&state, &cs, Some(&count_role)),
            Some(PlayerId(0)),
            "a CountSource role declares no recipient → the controller adds the mana"
        );

        // (c) Jeska's Will `Typed(Opponent)` count source → controller (P0).
        let opp = TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
        let jeska_role = ManaTargetRole::CountSource {
            count_source: opp.clone(),
        };
        let jeska = mk(count_source.clone(), jeska_role.clone());
        assert_eq!(
            mana_effect_recipient(&state, &jeska, Some(&jeska_role)),
            Some(PlayerId(0)),
            "Jeska's Will Typed(Opponent) count source → controller (unchanged)"
        );

        // (d) No role at all (Cabal Coffers) → the controller.
        let bare = ResolvedAbility::new(
            Effect::Mana {
                produced: recipient_prod.clone(),
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            src,
            PlayerId(0),
        );
        assert_eq!(
            mana_effect_recipient(&state, &bare, None),
            Some(PlayerId(0)),
            "no declared role → the controller adds the mana"
        );
    }

    /// CR 106.6 + CR 115.1: Jetfire's produced {C} carries the negative spend
    /// restriction "this mana can't be spent to cast nonartifact spells"
    /// (`SpellTypeOrAbilityActivation{ Artifact, Any }`), and it is deposited on
    /// the targeted player's mana units.
    #[test]
    fn target_player_recipient_mana_carries_spend_restriction() {
        use crate::types::mana::{AbilityActivationScope, ManaRestriction};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jetfire".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 2 },
                },
                restrictions: vec![ManaSpendRestriction::SpellTypeOrAbilityActivation {
                    spell_type: "Artifact".to_string(),
                    ability: AbilityActivationScope::Any,
                }],
                grants: vec![],
                expiry: None,
                target: Some(ManaTargetRole::Recipient {
                    recipient: TargetFilter::Player,
                }),
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].mana_pool.total(), 2);
        assert!(
            state.players[1].mana_pool.units().all(|unit| {
                unit.restrictions
                    .contains(&ManaRestriction::OnlyForTypeSpellsOrAbilities {
                        spell_type: "Artifact".to_string(),
                        ability: AbilityActivationScope::Any,
                    })
            }),
            "each produced {{C}} must carry the artifact-spell spend restriction"
        );
    }

    /// CR 106.1 + CR 106.5 + CR 202.2c: `AnyCombinationOfObjectColors` (Omnath,
    /// Locus of All) draws its colors from the target object. A monocolored
    /// target needs no prompt — `resolve` produces `count` mana of that color
    /// directly; a colorless target produces no mana (CR 106.5). (The multicolor
    /// prompt→produce flow is covered by the `omnath_tests` runtime suite.)
    #[test]
    fn any_combination_of_object_colors_uses_target_colors_and_empty_when_colorless() {
        let mk_ability = |target: ObjectId| {
            ResolvedAbility::new(
                Effect::Mana {
                    produced: ManaProduction::AnyCombinationOfObjectColors {
                        count: QuantityExpr::Fixed { value: 3 },
                        scope: crate::types::ability::ObjectScope::Target,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
                vec![TargetRef::Object(target)],
                ObjectId(100),
                PlayerId(0),
            )
        };

        // Monocolored (Black) target → three black mana, no prompt.
        let mut state = GameState::new_two_player(42);
        let mono = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "B".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&mono).unwrap().color = vec![ManaColor::Black];
        let mut events = Vec::new();
        resolve(&mut state, &mk_ability(mono), &mut events).unwrap();
        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseManaColor { .. }),
            "a single-color object needs no prompt"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 3);
        assert_eq!(state.players[0].mana_pool.total(), 3);

        // CR 106.5: colorless target → no prompt, no mana.
        let mut state2 = GameState::new_two_player(42);
        let colorless = create_object(
            &mut state2,
            CardId(2),
            PlayerId(0),
            "CL".to_string(),
            Zone::Hand,
        );
        state2.objects.get_mut(&colorless).unwrap().color = vec![];
        let mut events2 = Vec::new();
        resolve(&mut state2, &mk_ability(colorless), &mut events2).unwrap();
        assert_eq!(state2.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn firebending_marker_records_firebend_when_mana_is_produced() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Firebender".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().power = Some(4);
        let mut ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: crate::types::ability::ObjectScope::Source,
                        },
                    },
                    color_options: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: Some(crate::types::mana::ManaExpiry::EndOfCombat),
                target: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.may_trigger_origin = Some(MayTriggerOrigin::Keyword {
            keyword: crate::types::keywords::KeywordKind::Firebending,
        });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 4);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::Firebend {
                source_id,
                controller: PlayerId(0)
            } if *source_id == source
        )));
        assert!(state.players[0]
            .bending_types_this_turn
            .contains(&crate::types::events::BendingType::Fire));
    }

    #[test]
    fn firebending_marker_does_not_record_firebend_for_zero_mana() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Firebender".to_string(),
            Zone::Battlefield,
        );
        let mut ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 0 },
                    color_options: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: Some(crate::types::mana::ManaExpiry::EndOfCombat),
                target: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.may_trigger_origin = Some(MayTriggerOrigin::Keyword {
            keyword: crate::types::keywords::KeywordKind::Firebending,
        });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::Firebend { .. })));
    }

    #[test]
    fn produce_multiple_of_same_color() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green, ManaColor::Green, ManaColor::Green],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 3);
    }

    #[test]
    fn produce_event_context_amount_of_one_color() {
        let mut state = GameState::new_two_player(42);
        state.last_effect_count = Some(4);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyOneColor {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                color_options: vec![ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 4);
    }

    #[test]
    fn produce_empty_is_noop() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn produce_multi_color_fixed() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::White, ManaColor::Blue],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn emits_mana_added_per_unit() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Red, ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        let mana_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, GameEvent::ManaAdded { .. }))
            .collect();
        assert_eq!(mana_events.len(), 2);
    }

    #[test]
    fn emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Mana,
                ..
            }
        )));
    }

    #[test]
    fn empty_produced_adds_no_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn mana_units_track_source() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        let unit = &state.players[0].mana_pool.unit_at(0).unwrap();
        assert_eq!(unit.source_id, ObjectId(100));
    }

    #[test]
    fn produce_colorless_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 2 },
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            2
        );
    }

    #[test]
    fn any_one_color_effect_prompts_when_multiple_options() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 2 },
                color_options: vec![ManaColor::Blue, ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::SingleColor { options },
                context: ManaChoiceContext::ResolvingEffect(_),
                ..
            } => assert_eq!(options, &[ManaType::Blue, ManaType::Red]),
            other => panic!("expected SingleColor mana choice, got {other:?}"),
        }
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn any_combination_effect_prompts_and_resumes_sub_ability() {
        let mut state = GameState::new_two_player(42);
        let drawn = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Drawn Card".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let mut ability = make_mana_ability(ManaProduction::AnyCombination {
            count: QuantityExpr::Fixed { value: 2 },
            color_options: ManaColor::ALL.to_vec(),
        });
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )));

        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let (choice, pending_effect) = match state.waiting_for.clone() {
            WaitingFor::ChooseManaColor {
                player,
                choice: ManaChoicePrompt::AnyCombination { count, options },
                context: ManaChoiceContext::ResolvingEffect(pending_effect),
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 2);
                assert_eq!(
                    options,
                    vec![
                        ManaType::White,
                        ManaType::Blue,
                        ManaType::Black,
                        ManaType::Red,
                        ManaType::Green,
                    ]
                );
                (
                    ManaChoicePrompt::AnyCombination { count, options },
                    pending_effect,
                )
            }
            other => panic!("expected AnyCombination mana choice, got {other:?}"),
        };
        assert!(state.active_ability_continuation().is_some());

        handle_choose_mana_effect(
            &mut state,
            &pending_effect,
            PlayerId(0),
            &choice,
            None,
            ManaChoice::Combination(vec![ManaType::Red, ManaType::Green]),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
        assert!(state.players[0].hand.contains(&drawn));
        assert!(state.active_ability_continuation().is_none());
    }

    #[test]
    fn any_combination_effect_rejects_wrong_choice_count() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        let ability = make_mana_ability(ManaProduction::AnyCombination {
            count: QuantityExpr::Fixed { value: 3 },
            color_options: vec![ManaColor::Black, ManaColor::Green],
        });

        resolve(&mut state, &ability, &mut events).unwrap();
        let (choice, pending_effect) = match state.waiting_for.clone() {
            WaitingFor::ChooseManaColor {
                choice,
                context: ManaChoiceContext::ResolvingEffect(pending_effect),
                ..
            } => (choice, pending_effect),
            other => panic!("expected ChooseManaColor, got {other:?}"),
        };

        let result = handle_choose_mana_effect(
            &mut state,
            &pending_effect,
            PlayerId(0),
            &choice,
            None,
            ManaChoice::Combination(vec![ManaType::Black, ManaType::Green]),
            &mut events,
        );
        assert!(result.is_err());
    }

    #[test]
    fn chosen_color_resolves_from_object_attribute() {
        use crate::types::ability::ChosenAttribute;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let obj_id = ObjectId(100);
        let mut obj = crate::game::game_object::GameObject::new(
            obj_id,
            CardId(1),
            PlayerId(0),
            "Captivating Crossroads".to_string(),
            Zone::Battlefield,
        );
        obj.chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Green));
        state.objects.insert(obj_id, obj);

        let mut events = Vec::new();
        let ability = make_mana_ability(ManaProduction::ChosenColor {
            count: QuantityExpr::Fixed { value: 1 },
            contribution: ManaContribution::Base,
            fixed_alternative: None,
        });
        // Override source_id to match our object
        let ability = ResolvedAbility {
            source_id: obj_id,
            ..ability
        };

        resolve(&mut state, &ability, &mut events).unwrap();

        let player = state.players.iter().find(|p| p.id == PlayerId(0)).unwrap();
        assert_eq!(player.mana_pool.count_color(ManaType::Green), 1);
    }

    #[test]
    fn chosen_color_dynamic_count_reads_current_named_choice() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;
        use crate::types::mana::{ManaCost, ManaCostShard};
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Nyx Lotus".to_string(),
            Zone::Battlefield,
        );
        let permanent = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Green Permanent".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&permanent).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 1,
        };
        state.last_named_choice = Some(ChoiceValue::Color(ManaColor::Green));

        let mut events = Vec::new();
        let ability = ResolvedAbility {
            source_id,
            ..make_mana_ability(ManaProduction::ChosenColor {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Devotion {
                        colors: DevotionColors::ChosenColor,
                    },
                },
                contribution: ManaContribution::Base,
                fixed_alternative: None,
            })
        };

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 2);
    }

    #[test]
    fn chosen_color_unresolved_is_noop() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::ChosenColor {
                count: QuantityExpr::Fixed { value: 1 },
                contribution: ManaContribution::Base,
                fixed_alternative: None,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn chosen_color_count_derivation_independent_of_color() {
        // Issue #482 Defect B: a `ChosenColor` with `fixed_alternative: Some(_)`
        // and no chosen color must still derive `count == 1` — the count
        // derivation cannot depend on a color being resolvable.
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Manor Gate".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility {
            source_id,
            ..make_mana_ability(ManaProduction::ChosenColor {
                count: QuantityExpr::Fixed { value: 1 },
                contribution: ManaContribution::Base,
                fixed_alternative: Some(ManaColor::Green),
            })
        };
        let produced = ManaProduction::ChosenColor {
            count: QuantityExpr::Fixed { value: 1 },
            contribution: ManaContribution::Base,
            fixed_alternative: Some(ManaColor::Green),
        };
        let types = resolve_mana_types_for_ability(&produced, &state, &ability);
        assert_eq!(
            types.len(),
            1,
            "count must derive to 1 even with no chosen color"
        );
        // No-prompt default path produces the fixed color deterministically.
        assert_eq!(types[0], ManaType::Green);
    }

    #[test]
    fn gate_land_mana_ability_offers_fixed_or_chosen() {
        // Issue #482 Defect B: Manor Gate's "{T}: Add {G} or one mana of the
        // chosen color" — once a color (Red) is chosen, the resolver supplied a
        // SingleColor choice must produce exactly the selected color, exactly
        // once, for either option.
        use crate::game::zones::create_object;
        use crate::types::ability::ChosenAttribute;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Manor Gate".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Red));

        let produced = ManaProduction::ChosenColor {
            count: QuantityExpr::Fixed { value: 1 },
            contribution: ManaContribution::Base,
            fixed_alternative: Some(ManaColor::Green),
        };
        let ability = ResolvedAbility {
            source_id,
            ..make_mana_ability(produced.clone())
        };

        // Each choice in the SingleColor prompt yields exactly that color once.
        for chosen in [ManaType::Green, ManaType::Red] {
            let prompt = ManaChoicePrompt::SingleColor {
                options: vec![ManaType::Green, ManaType::Red],
            };
            let types = chosen_mana_types_for_prompt(
                &state,
                &ability,
                &ability,
                &produced,
                &prompt,
                ManaChoice::SingleColor(chosen),
            )
            .unwrap();
            assert_eq!(types, vec![chosen], "chosen color produced exactly once");
        }
    }

    #[test]
    fn opponent_land_colors_produces_from_opponent_lands() {
        // CR 106.7: Mana of any color that a land an opponent controls could produce.
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind};
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // Opponent (PlayerId(1)) has a Mountain on the battlefield with a red mana ability.
        let mountain = create_object(
            &mut state,
            CardId(201),
            PlayerId(1),
            "Mountain".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&mountain).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Mountain".to_string());
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Red],
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

        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            }),
            &mut events,
        )
        .unwrap();

        // Should produce red mana (from opponent's Mountain).
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    /// CR 106.7 (issue #1556): Exotic Orchard — "Add one mana of any color that a
    /// land an opponent controls could produce." When the opponent's lands could
    /// produce more than one color, the activator must be prompted to choose,
    /// not silently handed the first color. Mirrors `AnyTypeProduceableBy`
    /// (Reflecting Pool) prompt behavior.
    #[test]
    fn opponent_land_colors_prompts_choice_when_multiple_colors_available() {
        let mut state = GameState::new_two_player(42);

        // Player 0 controls the Exotic Orchard — the prompt reads the source's controller.
        let orchard = create_object(
            &mut state,
            CardId(401),
            PlayerId(0),
            "Exotic Orchard".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&orchard)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        // Opponent (PlayerId(1)) controls a Mountain (red) and a Forest (green).
        for (cid, name, color, sub) in [
            (402u64, "Mountain", ManaColor::Red, "Mountain"),
            (403u64, "Forest", ManaColor::Green, "Forest"),
        ] {
            let land = create_object(
                &mut state,
                CardId(cid),
                PlayerId(1),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push(sub.to_string());
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![color],
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

        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::OpponentLandColors {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            orchard,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The activator must be asked which color — not handed the first one.
        match &state.waiting_for {
            crate::types::game_state::WaitingFor::ChooseManaColor {
                choice: crate::types::game_state::ManaChoicePrompt::SingleColor { options },
                ..
            } => {
                assert!(options.contains(&ManaType::Red), "red should be offered");
                assert!(
                    options.contains(&ManaType::Green),
                    "green should be offered"
                );
                assert_eq!(options.len(), 2);
            }
            other => panic!("expected a ChooseManaColor SingleColor prompt, got {other:?}"),
        }
        // No mana enters the pool until the choice is made.
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn opponent_land_colors_no_opponent_lands_produces_nothing() {
        // CR 106.5 + CR 106.7: If no color can be defined, produce no mana.
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn opponent_land_colors_mirror_exotic_orchard_no_recursion() {
        // CR 106.7: Two opposing Exotic Orchards with no other lands —
        // neither can define a color, so both produce no mana (no infinite recursion).
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind};
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // Opponent (PlayerId(1)) has an Exotic Orchard (OpponentLandColors ability).
        let opp_orchard = create_object(
            &mut state,
            CardId(301),
            PlayerId(1),
            "Exotic Orchard".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&opp_orchard).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::OpponentLandColors {
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

        // Player 0 activates their own OpponentLandColors ability.
        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            }),
            &mut events,
        )
        .unwrap();

        // No recursion; opponent's Exotic Orchard is skipped, so no colors available.
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn restriction_spell_type_attaches_to_produced_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![ManaSpendRestriction::SpellType("Creature".to_string())],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let unit = &state.players[0].mana_pool.unit_at(0).unwrap();
        assert_eq!(unit.restrictions.len(), 1);
        assert_eq!(
            unit.restrictions[0],
            ManaRestriction::OnlyForSpellType("Creature".to_string())
        );
    }

    #[test]
    fn restriction_chosen_creature_type_resolves_from_source() {
        use crate::types::ability::ChosenAttribute;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let obj_id = ObjectId(200);
        let mut obj = crate::game::game_object::GameObject::new(
            obj_id,
            CardId(2),
            PlayerId(0),
            "Cavern of Souls".to_string(),
            Zone::Battlefield,
        );
        obj.chosen_attributes
            .push(ChosenAttribute::CreatureType("Elf".to_string()));
        state.objects.insert(obj_id, obj);

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![ManaSpendRestriction::ChosenCreatureType],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let unit = &state.players[0].mana_pool.unit_at(0).unwrap();
        assert_eq!(unit.restrictions.len(), 1);
        assert_eq!(
            unit.restrictions[0],
            ManaRestriction::OnlyForCreatureType("Elf".to_string())
        );
    }

    #[test]
    fn restriction_chosen_creature_type_drops_when_no_choice() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![ManaSpendRestriction::ChosenCreatureType],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // No source object → restriction can't resolve → mana is unrestricted
        let unit = &state.players[0].mana_pool.unit_at(0).unwrap();
        assert!(unit.restrictions.is_empty());
    }

    #[test]
    fn grants_flow_through_to_mana_unit() {
        use crate::types::mana::ManaSpellGrant;

        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![ManaSpellGrant::CantBeCountered {
                    filter: TargetFilter::Any,
                }],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let unit = &state.players[0].mana_pool.unit_at(0).unwrap();
        assert_eq!(
            unit.grants,
            vec![ManaSpellGrant::CantBeCountered {
                filter: TargetFilter::Any,
            }]
        );
    }

    /// CR 106.7 + CR 106.1b: Reflecting Pool — produces one mana of any type
    /// that a land you control could produce. With a Plains and a Swamp on the
    /// battlefield, the type union is {W, B}; the resolver picks the first
    /// listed type when no choice override is supplied (mirrors `AnyOneColor`).
    #[test]
    fn any_type_produceable_by_you_control_unions_types() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // Player 0 controls a Plains and a Swamp.
        for (card_id, name, color, subtype) in [
            (CardId(401), "Plains", ManaColor::White, "Plains"),
            (CardId(402), "Swamp", ManaColor::Black, "Swamp"),
        ] {
            let id = create_object(
                &mut state,
                card_id,
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push(subtype.to_string());
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![color],
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

        let land_filter = TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));
        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyTypeProduceableBy {
                count: QuantityExpr::Fixed { value: 1 },
                land_filter,
            }),
            &mut events,
        )
        .unwrap();

        let crate::types::game_state::WaitingFor::ChooseManaColor {
            choice: crate::types::game_state::ManaChoicePrompt::SingleColor { options },
            ..
        } = &state.waiting_for
        else {
            panic!("the land-type union offers its public mana choice");
        };
        assert!(options.contains(&ManaType::White));
        assert!(options.contains(&ManaType::Black));
        assert_eq!(options.len(), 2);
        let color = options[0];
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseManaColor {
                choice: crate::types::game_state::ManaChoice::SingleColor(color),
                count: 1,
            },
        )
        .expect("choose an actually offered mana type");

        // CR 106.7: the selected mana must belong to the complete type union.
        assert_eq!(state.players[0].mana_pool.total(), 1);
        let white = state.players[0].mana_pool.count_color(ManaType::White);
        let black = state.players[0].mana_pool.count_color(ManaType::Black);
        assert_eq!(
            white + black,
            1,
            "produced mana must come from the {{W,B}} type union (got W={white}, B={black})"
        );

        // The full type union (helper-level) must include both colors.
        let options = crate::game::mana_sources::produceable_mana_types_by_filter(
            &state,
            &TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You)),
            PlayerId(0),
            ObjectId(100),
            None,
        );
        assert!(options.contains(&ManaType::White), "union must include W");
        assert!(options.contains(&ManaType::Black), "union must include B");
    }

    /// CR 106.5 + CR 106.7: When no land matches the filter, the type union is
    /// empty, so the ability produces no mana.
    #[test]
    fn any_type_produceable_by_empty_union_produces_nothing() {
        use crate::types::ability::{ControllerRef, TargetFilter, TypedFilter};

        let mut state = GameState::new_two_player(42);
        let land_filter = TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyTypeProduceableBy {
                count: QuantityExpr::Fixed { value: 1 },
                land_filter,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    /// CR 106.7: Two Reflecting Pools facing each other (no other lands) — the
    /// recursive `AnyTypeProduceableBy` skip prevents infinite recursion and
    /// the union collapses to empty (CR 106.5 — no mana).
    #[test]
    fn any_type_produceable_by_recursive_yields_empty() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let recursive_filter =
            TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));

        // Player 0 has a Reflecting Pool already on the battlefield.
        let pool = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Reflecting Pool".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&pool).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyTypeProduceableBy {
                        count: QuantityExpr::Fixed { value: 1 },
                        land_filter: recursive_filter.clone(),
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyTypeProduceableBy {
                count: QuantityExpr::Fixed { value: 1 },
                land_filter: recursive_filter,
            }),
            &mut events,
        )
        .unwrap();

        // Both producers are recursive; no other lands → empty union → no mana.
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    /// CR 106.1b: Reflecting Pool reads "any **type**" — a Wastes you control
    /// (which produces colorless) must contribute `Colorless` to the union.
    #[test]
    fn any_type_produceable_by_includes_colorless() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // Player 0 controls a Wastes (produces {C}).
        let wastes = create_object(
            &mut state,
            CardId(601),
            PlayerId(0),
            "Wastes".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&wastes).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
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

        let land_filter = TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));
        let options = crate::game::mana_sources::produceable_mana_types_by_filter(
            &state,
            &land_filter,
            PlayerId(0),
            ObjectId(9999),
            None,
        );
        assert!(
            options.contains(&ManaType::Colorless),
            "type union must include Colorless when a Wastes is controlled (CR 106.1b)"
        );
    }

    /// CR 106.7 + CR 106.5: P0 controls Exotic Orchard (`OpponentLandColors`),
    /// P1 controls Reflecting Pool (`AnyTypeProduceableBy`), and neither
    /// player controls any other land. The mutual recursion guard must be
    /// symmetric — `opponent_land_color_options` skips both recursive
    /// producers, so the survey terminates with the empty set rather than
    /// re-anchoring `ControllerRef::You` to the wrong player or looping.
    /// Activating either side produces no mana per CR 106.5.
    #[test]
    fn exotic_orchard_with_opponent_reflecting_pool_no_panic() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // P0 controls Exotic Orchard.
        let orchard = create_object(
            &mut state,
            CardId(701),
            PlayerId(0),
            "Exotic Orchard".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&orchard).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::OpponentLandColors {
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

        // P1 controls Reflecting Pool.
        let pool = create_object(
            &mut state,
            CardId(702),
            PlayerId(1),
            "Reflecting Pool".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&pool).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyTypeProduceableBy {
                        count: QuantityExpr::Fixed { value: 1 },
                        land_filter: TargetFilter::Typed(
                            TypedFilter::land().controller(ControllerRef::You),
                        ),
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        // P0's Exotic Orchard surveys P1's lands → only finds Reflecting Pool
        // (recursive — skipped) → empty set.
        let orchard_opts =
            crate::game::mana_sources::opponent_land_color_options(&state, PlayerId(0));
        assert!(
            orchard_opts.is_empty(),
            "Exotic Orchard facing only an opponent's Reflecting Pool must yield empty (CR 106.5); got {orchard_opts:?}"
        );

        // P1's Reflecting Pool surveys P1's lands → only itself (recursive,
        // skipped) → empty set. (Cross-controller cycle terminates cleanly.)
        let pool_opts = crate::game::mana_sources::produceable_mana_types_by_filter(
            &state,
            &TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You)),
            PlayerId(1),
            pool,
            None,
        );
        assert!(
            pool_opts.is_empty(),
            "Reflecting Pool with no other own lands must yield empty (CR 106.5); got {pool_opts:?}"
        );

        // Both should activate without panic and produce zero mana.
        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            }),
            &mut events,
        )
        .unwrap();
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    /// Builds a two-player Commander board for issue #9525: P0 (the surveyor)
    /// has a black/red commander, P1 has a mono-blue commander and controls a
    /// Command Tower. The commanders deliberately share no color, so a survey
    /// that reads the wrong player's commander produces visibly wrong mana.
    fn commander_board_with_opponent_command_tower() -> GameState {
        let mut state = GameState::new_two_player(42);

        for (card_id, owner, name, colors) in [
            (
                CardId(501),
                PlayerId(0),
                "Rakdos, Lord of Riots",
                vec![ManaColor::Black, ManaColor::Red],
            ),
            (
                CardId(502),
                PlayerId(1),
                "Talrand, Sky Summoner",
                vec![ManaColor::Blue],
            ),
        ] {
            let commander =
                create_object(&mut state, card_id, owner, name.to_string(), Zone::Command);
            let obj = state.objects.get_mut(&commander).unwrap();
            obj.is_commander = true;
            obj.base_color = colors.clone();
            obj.color = colors;
        }

        let tower = create_object(
            &mut state,
            CardId(503),
            PlayerId(1),
            "Command Tower".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&tower).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyInCommandersColorIdentity {
                        count: QuantityExpr::Fixed { value: 1 },
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

        state
    }

    /// CR 106.7 + CR 109.5 (issue #9525): Fellwar Stone / Exotic Orchard asks
    /// what an opponent's Command Tower could produce. "Your commander" on the
    /// tower means its controller's commander, so the survey yields only blue.
    ///
    /// Revert-failing discriminator: evaluating the tower as the activator reads
    /// P0's black/red commander, offering {B, R} and producing no blue.
    #[test]
    fn opponent_land_colors_reads_command_tower_for_its_controller() {
        let mut state = commander_board_with_opponent_command_tower();

        let options = crate::game::mana_sources::opponent_land_color_options(&state, PlayerId(0));
        assert_eq!(options, vec![ManaColor::Blue]);

        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            }),
            &mut events,
        )
        .unwrap();
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    /// CR 106.7 + CR 109.5 (issue #9525): the filter-scoped survey
    /// (`AnyTypeProduceableBy`) evaluates each admitted land for its own
    /// controller too, so an opponent-scoped filter over a Command Tower
    /// yields that opponent's commander colors, not the activator's.
    #[test]
    fn produceable_types_by_opponent_filter_reads_command_tower_for_its_controller() {
        let state = commander_board_with_opponent_command_tower();

        let options = crate::game::mana_sources::produceable_mana_types_by_filter(
            &state,
            &TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::Opponent)),
            PlayerId(0),
            ObjectId(100),
            None,
        );
        assert_eq!(options, vec![ManaType::Blue]);
    }

    #[test]
    fn colorless_production_counts_creatures_sharing_type_with_triggering_source() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            ControllerRef, FilterProp, QuantityExpr, QuantityRef, SharedQuality,
            SharedQualityRelation, TypedFilter,
        };
        use crate::types::events::GameEvent;
        use crate::types::game_state::ZoneChangeRecord;
        use crate::types::identifiers::CardId;
        use crate::types::player::PlayerId;

        let mut state = GameState::new_two_player(42);
        state.all_creature_types = vec!["Goblin".to_string()];
        let mana_echoes = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mana Echoes".to_string(),
            Zone::Battlefield,
        );
        let goblin_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Goblin A".to_string(),
            Zone::Battlefield,
        );
        let goblin_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Goblin B".to_string(),
            Zone::Battlefield,
        );
        let entering = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Goblin C".to_string(),
            Zone::Battlefield,
        );
        for id in [goblin_a, goblin_b, entering] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Goblin".to_string());
        }

        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: entering,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord::test_minimal(
                entering,
                Some(Zone::Hand),
                Zone::Battlefield,
            )),
        });

        let filter = TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::SharesQuality {
                    quality: SharedQuality::CreatureType,
                    reference: Some(Box::new(TargetFilter::TriggeringSource)),
                    relation: SharedQualityRelation::Shares,
                }]),
        );
        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: filter.clone(),
                        },
                    },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            mana_echoes,
            PlayerId(0),
        );

        let produced = super::resolve_mana_types_for_ability(
            &ManaProduction::Colorless {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { filter },
                },
            },
            &state,
            &ability,
        );
        assert_eq!(
            produced.len(),
            3,
            "Mana Echoes must produce one colorless per matching creature"
        );
    }
    // Supplemental canonical composition; these are not printed card abilities.
    // Colored noncreatures are typed canonical witnesses; Forest/Mountain use the existing basic-land API.
    fn departure_dynamic_mana_runner(
        shape: u8,
        foreign_seven: bool,
        empty: bool,
    ) -> (
        crate::game::scenario::GameRunner,
        ObjectId,
        ObjectId,
        ObjectId,
    ) {
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::ability::{
            AggregateFunction, Comparator, FilterProp, ObjectProperty, ReturnResultId,
            ZoneChangeRecordSource,
        };
        use crate::types::mana::{ManaCost, ManaCostShard, ManaUnit};
        use crate::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mut victims = Vec::new();
        if !empty {
            victims.push(scenario.add_creature(P0, "Centaur Courser", 3, 3).id());
            victims.push(
                scenario
                    .add_creature(
                        P0,
                        if foreign_seven {
                            "Rumbling Baloth"
                        } else {
                            "Grizzly Bears"
                        },
                        if foreign_seven { 4 } else { 2 },
                        if foreign_seven { 4 } else { 2 },
                    )
                    .id(),
            );
            victims.push(scenario.add_creature(P1, "Grizzly Bears", 2, 2).id());
        }
        if shape < 2 || shape == 6 {
            let mv = if empty {
                0
            } else if foreign_seven {
                7
            } else {
                5
            } + usize::from(shape == 6);
            for (name, color) in [
                ("Supplemental Red noncreature", ManaColor::Red),
                ("Supplemental Green noncreature", ManaColor::Green),
            ] {
                scenario
                    .add_artifact_from_oracle(P0, name, "")
                    .with_mana_cost(ManaCost::Cost {
                        generic: mv as u32,
                        shards: vec![],
                    })
                    .with_color(vec![color]);
            }
            scenario
                .add_artifact_from_oracle(
                    if shape == 6 { P0 } else { P1 },
                    "Supplemental Blue decoy",
                    "",
                )
                .with_mana_cost(ManaCost::Cost {
                    generic: if shape == 6 { (mv + 1) as u32 } else { 0 },
                    shards: vec![],
                })
                .with_color(vec![ManaColor::Blue]);
        } else if shape <= 3 {
            if shape == 2 {
                scenario.add_basic_land(P0, ManaColor::Red);
            }
            scenario.add_basic_land(P0, ManaColor::Green);
            scenario
                .add_artifact_from_oracle(P0, "Supplemental nonland color decoy", "")
                .with_color(vec![ManaColor::Blue]);
        }
        if shape == 6 {
            scenario.add_card_to_hand(P1, "Recipient hand witness");
        }
        for player in [P0, P1] {
            for _ in 0..8 {
                scenario.add_card_to_library_top(player, "Library witness");
            }
        }
        let mut eligible = TypedFilter::default().controller(ControllerRef::You);
        eligible.properties.push(FilterProp::Cmc {
            comparator: if shape < 2 || shape == 6 {
                Comparator::EQ
            } else {
                Comparator::LT
            },
            value: QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeAggregate {
                    source: ZoneChangeRecordSource::InstructionResult {
                        result: ReturnResultId(7),
                    },
                    from: Some(Zone::Battlefield),
                    to: Some(Zone::Exile),
                    filter: TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::SpecificPlayer { id: P0 }),
                    ),
                    function: AggregateFunction::Sum,
                    property: ObjectProperty::Power,
                },
            },
        });
        if shape == 7 {
            let FilterProp::Cmc {
                value:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ZoneChangeAggregate {
                                filter: TargetFilter::Typed(former),
                                ..
                            },
                    },
                ..
            } = &mut eligible.properties[0]
            else {
                unreachable!()
            };
            former.controller = Some(ControllerRef::ScopedPlayer);
        }
        let FilterProp::Cmc {
            value: addressed_count,
            ..
        } = eligible.properties[0].clone()
        else {
            unreachable!()
        };
        let hand_count = QuantityExpr::Ref {
            qty: QuantityRef::TargetZoneCardCount {
                zone: crate::types::ability::ZoneRef::Hand,
                scope: ControllerRef::TargetPlayer,
                binding: crate::types::ability::CountBinding::Explicit,
            },
        };
        if shape == 6 {
            let FilterProp::Cmc { value, .. } = &mut eligible.properties[0] else {
                unreachable!()
            };
            *value = QuantityExpr::Sum {
                exprs: vec![addressed_count.clone(), hand_count.clone()],
            };
        }
        let filter = TargetFilter::Typed(eligible);
        let produced = match shape {
            0 => ManaProduction::AnyOneColorAmongPermanents {
                count: QuantityExpr::Fixed { value: 1 },
                filter,
                contribution: ManaContribution::Base,
            },
            1 => ManaProduction::DistinctColorsAmongPermanents { filter },
            2 | 3 => ManaProduction::AnyTypeProduceableBy {
                count: QuantityExpr::Fixed { value: 1 },
                land_filter: filter,
            },
            6 => ManaProduction::AnyOneColorAmongPermanents {
                count: hand_count,
                filter,
                contribution: ManaContribution::Base,
            },
            4 | 7 | 8 => ManaProduction::AnyOneColor {
                count: addressed_count,
                color_options: vec![ManaColor::Red, ManaColor::Green],
                contribution: ManaContribution::Base,
            },
            5 => ManaProduction::AnyCombination {
                count: addressed_count,
                color_options: vec![ManaColor::Red, ManaColor::Green],
            },
            9 => ManaProduction::ChoiceAmongCombinations {
                options: vec![
                    vec![ManaColor::Red, ManaColor::Red],
                    vec![ManaColor::Green, ManaColor::Green],
                ],
            },
            _ => unreachable!(),
        };
        let mut mana = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Mana {
                produced,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: if shape == 6 {
                    Some(ManaTargetRole::Both {
                        recipient: TargetFilter::Player,
                        count_source: TargetFilter::Player,
                    })
                } else {
                    None
                },
            },
        );
        if shape == 7 {
            mana.player_scope = Some(crate::types::ability::PlayerFilter::All);
        }
        if shape == 8 {
            let mut next = mana.clone();
            next.player_scope = Some(crate::types::ability::PlayerFilter::Opponent);
            let Effect::Mana {
                produced:
                    ManaProduction::AnyOneColor {
                        count:
                            QuantityExpr::Ref {
                                qty:
                                    QuantityRef::ZoneChangeAggregate {
                                        filter: TargetFilter::Typed(former),
                                        ..
                                    },
                            },
                        ..
                    },
                ..
            } = next.effect.as_mut()
            else {
                unreachable!()
            };
            former.controller = Some(ControllerRef::SpecificPlayer { id: P1 });
            mana.sub_ability = Some(Box::new(next));
        }
        let mut producer: Effect = serde_json::from_value(serde_json::json!({"type":"ChangeZoneAll", "origin":"Battlefield", "destination":"Exile"})).unwrap();
        let Effect::ChangeZoneAll { target, .. } = &mut producer else {
            unreachable!()
        };
        *target = TargetFilter::Typed(TypedFilter::creature());
        let mut root = AbilityDefinition::new(AbilityKind::Spell, producer);
        root.declares_return_result = Some(ReturnResultId(7));
        root.sub_ability = Some(Box::new(mana));
        let spell = scenario
            .add_spell_to_hand(P0, "Supplemental dynamic mana process", false)
            .with_ability_definition(root)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![],
            })
            .id();
        let payment_effect = || {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    player: TargetFilter::SpecificPlayer { id: P0 },
                    amount: QuantityExpr::Fixed { value: 3 },
                },
            )
        };
        let tagged = scenario
            .add_spell_to_hand(P0, "Supplemental qualified mana payment", false)
            .with_ability_definition(payment_effect())
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::TwoOrMoreColorSource],
            })
            .id();
        let ordinary = scenario
            .add_spell_to_hand(P0, "Supplemental ordinary green payment", false)
            .with_ability_definition(payment_effect())
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
        );
        let mut runner = scenario.build();
        if foreign_seven {
            runner.state_mut().next_return_result_occurrence_id = 19;
            runner.state_mut().next_return_result_process_id = 37;
        }
        if shape == 6 {
            runner.cast(spell).target_players(&[P1, P0]).commit();
        } else {
            runner.cast(spell).commit();
        }
        for _ in 0..8 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseManaColor { .. }
            ) || runner.state().objects[&spell].zone == Zone::Graveyard
            {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            runner
                .act(crate::types::actions::GameAction::PassPriority)
                .unwrap();
        }
        let records: Vec<_> = runner
            .state()
            .zone_changes_this_turn
            .iter()
            .filter(|record| victims.contains(&record.object_id))
            .collect();
        assert_eq!(records.len(), victims.len());
        assert_eq!(
            records
                .iter()
                .filter(|record| record.controller == P0)
                .map(|record| record.power.unwrap())
                .sum::<i32>(),
            if empty {
                0
            } else if foreign_seven {
                7
            } else {
                5
            }
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| record.controller == P1)
                .map(|record| record.power.unwrap())
                .sum::<i32>(),
            if empty { 0 } else { 2 }
        );
        for id in victims {
            assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
        }
        (runner, spell, tagged, ordinary)
    }

    fn assert_departure_mana_completion(
        runner: &crate::game::scenario::GameRunner,
        spell: ObjectId,
    ) {
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(runner.state().return_result_frames.is_empty());
    }

    #[test]
    fn departure_dynamic_color_prompt_qualifies_the_actual_unit_for_public_payment() {
        use crate::game::scenario::{P0, P1};
        use crate::types::actions::GameAction;
        for shape in [0, 2] {
            let (mut runner, spell, tagged, _) = departure_dynamic_mana_runner(shape, false, false);
            let WaitingFor::ChooseManaColor {
                player,
                choice: ManaChoicePrompt::SingleColor { options },
                return_result_occurrence: Some(occurrence),
                context: ManaChoiceContext::ResolvingEffect(ability),
            } = &runner.state().waiting_for
            else {
                panic!("final dynamic-filter prompt must retain the result");
            };
            assert_eq!(*player, P0);
            assert_eq!(options, &[ManaType::Red, ManaType::Green]);
            assert_eq!(
                runner.state().return_result_frames[occurrence].process,
                ability.return_result_process
            );
            assert!(ability.sub_ability.is_none());
            assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            for projected in [
                crate::game::visibility::filter_state_for_viewer(runner.state(), P0),
                crate::game::visibility::filter_state_for_viewer(runner.state(), P1),
                crate::game::visibility::filter_state_for_unseated_viewer(runner.state()),
            ] {
                assert!(projected.return_result_frames.is_empty());
                assert!(!serde_json::to_string(&projected)
                    .unwrap()
                    .contains("return_result_process"));
            }
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() =
                serde_json::from_value(saved).expect("sole final mana owner reloads");
            runner
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(ManaType::Green),
                    count: 1,
                })
                .unwrap();
            assert_departure_mana_completion(&runner, spell);
            let units: Vec<_> = runner.state().players[0].mana_pool.units().collect();
            assert_eq!(units.len(), 1);
            assert_eq!(units[0].color, ManaType::Green);
            assert_eq!(units[0].source_id, spell);
            assert_ne!(units[0].pip_id.0, 0);
            assert!(units[0].source_could_produce_two_or_more_colors);
            let paid_pip = units[0].pip_id;
            runner.cast(tagged).resolve();
            assert_eq!(runner.state().players[0].life, 23);
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert!(!runner.state().players[0]
                .mana_pool
                .units()
                .any(|unit| unit.pip_id == paid_pip));
            assert_departure_mana_completion(&runner, tagged);
        }
    }

    #[test]
    fn departure_dynamic_immediate_and_single_land_paths_use_the_bound_filter() {
        use crate::game::scenario::GameRunner;
        let (distinct, spell, _, _) = departure_dynamic_mana_runner(1, false, false);
        assert_departure_mana_completion(&distinct, spell);
        let units: Vec<_> = distinct.state().players[0].mana_pool.units().collect();
        assert_eq!(
            units.iter().map(|unit| unit.color).collect::<Vec<_>>(),
            vec![ManaType::Red, ManaType::Green]
        );
        assert!(units
            .iter()
            .all(|unit| unit.source_id == spell && unit.source_could_produce_two_or_more_colors));
        let (one_land, spell, tagged, ordinary) = departure_dynamic_mana_runner(3, false, false);
        assert_departure_mana_completion(&one_land, spell);
        let units: Vec<_> = one_land.state().players[0].mana_pool.units().collect();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].color, ManaType::Green);
        assert_eq!(units[0].source_id, spell);
        assert!(!units[0].source_could_produce_two_or_more_colors);
        let mut rejected = GameRunner::from_state(one_land.state().clone());
        assert!(
            rejected.cast(tagged).try_resolve().is_err(),
            "an ordinary Green unit cannot pay the qualified-source shard"
        );
        assert_eq!(rejected.state().players[0].life, 20);
        let mut normal = one_land;
        normal.cast(ordinary).resolve();
        assert_eq!(normal.state().players[0].life, 23);
        assert_departure_mana_completion(&normal, ordinary);
        for shape in 0..4 {
            let (mut empty, spell, _, _) = departure_dynamic_mana_runner(shape, false, true);
            if shape == 0 {
                assert!(
                    matches!(&empty.state().waiting_for, WaitingFor::ChooseManaColor { choice: ManaChoicePrompt::SingleColor { options }, .. } if options == &[ManaType::Red, ManaType::Green])
                );
                let saved = serde_json::to_value(empty.state()).unwrap();
                *empty.state_mut() = serde_json::from_value(saved)
                    .expect("completed empty still supplies exact EQ0 history");
                empty
                    .act(crate::types::actions::GameAction::ChooseManaColor {
                        choice: ManaChoice::SingleColor(ManaType::Green),
                        count: 1,
                    })
                    .unwrap();
            }
            assert_departure_mana_completion(&empty, spell);
            assert_eq!(
                empty.state().players[0].mana_pool.total(),
                match shape {
                    0 => 1,
                    1 => 2,
                    _ => 0,
                }
            );
            if shape < 2 {
                assert!(empty.state().players[0]
                    .mana_pool
                    .units()
                    .all(|unit| unit.source_could_produce_two_or_more_colors));
            }
        }
    }

    #[test]
    fn departure_dynamic_mana_response_rejects_foreign_process_before_filter_or_deposit() {
        use crate::game::scenario::GameRunner;
        use crate::types::ability::ReturnResultId;
        use crate::types::actions::GameAction;
        use crate::types::game_state::{InstructionResult, RevealedInstructionResult};
        for shape in [0, 2, 4, 6] {
            let (mut foreign, foreign_spell, _, _) =
                departure_dynamic_mana_runner(shape, true, false);
            let WaitingFor::ChooseManaColor {
                return_result_occurrence: Some(foreign_occurrence),
                context: ManaChoiceContext::ResolvingEffect(ability),
                ..
            } = &foreign.state().waiting_for
            else {
                panic!("genuine foreign seven prompt reached");
            };
            let foreign_occurrence = *foreign_occurrence;
            let foreign_frame = foreign.state().return_result_frames[&foreign_occurrence].clone();
            assert_eq!(foreign_frame.process, ability.return_result_process);
            foreign
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(ManaType::Green),
                    count: 1,
                })
                .unwrap();
            assert_departure_mana_completion(&foreign, foreign_spell);
            assert_eq!(
                foreign.state().players[usize::from(shape == 6)]
                    .mana_pool
                    .total(),
                if shape == 4 {
                    7
                } else if shape == 6 {
                    2
                } else {
                    1
                }
            );
            let (positive, spell, _, _) = departure_dynamic_mana_runner(shape, false, false);
            let WaitingFor::ChooseManaColor {
                return_result_occurrence: Some(original),
                context: ManaChoiceContext::ResolvingEffect(ability),
                ..
            } = &positive.state().waiting_for
            else {
                unreachable!()
            };
            let original = *original;
            let expected = ability.return_result_process;
            assert!(expected.is_some());
            assert_ne!(foreign_frame.process, expected);
            for fault in 0..7 {
                let mut hostile = positive.state().clone();
                match fault {
                    0 => {
                        hostile.return_result_frames.remove(&original);
                        hostile
                            .return_result_frames
                            .insert(foreign_occurrence, foreign_frame.clone());
                        hostile.next_return_result_occurrence_id = foreign_occurrence.0 + 1;
                        hostile.next_return_result_process_id =
                            foreign_frame.process.unwrap().0 + 1;
                        let WaitingFor::ChooseManaColor {
                            return_result_occurrence,
                            context: ManaChoiceContext::ResolvingEffect(ability),
                            ..
                        } = &mut hostile.waiting_for
                        else {
                            unreachable!()
                        };
                        *return_result_occurrence = Some(foreign_occurrence);
                        assert_eq!(
                            ability.return_result_process, expected,
                            "lookup mutation cannot supply its expected authority"
                        );
                    }
                    1 => {
                        hostile.return_result_frames.clear();
                    }
                    2 => {
                        hostile
                            .return_result_frames
                            .get_mut(&original)
                            .unwrap()
                            .results
                            .remove(&ReturnResultId(7));
                    }
                    3 => {
                        hostile
                            .return_result_frames
                            .get_mut(&original)
                            .unwrap()
                            .results
                            .insert(
                                ReturnResultId(7),
                                InstructionResult::RevealedCards(RevealedInstructionResult {
                                    revealed_cards: vec![],
                                }),
                            );
                    }
                    4 => {
                        let WaitingFor::ChooseManaColor {
                            context: ManaChoiceContext::ResolvingEffect(ability),
                            ..
                        } = &mut hostile.waiting_for
                        else {
                            unreachable!()
                        };
                        ability.return_result_process = None;
                    }
                    5 => {
                        let WaitingFor::ChooseManaColor {
                            return_result_occurrence,
                            ..
                        } = &mut hostile.waiting_for
                        else {
                            unreachable!()
                        };
                        *return_result_occurrence = None;
                    }
                    6 => {
                        let InstructionResult::ZoneChanges(records) = hostile
                            .return_result_frames
                            .get_mut(&original)
                            .unwrap()
                            .results
                            .get_mut(&ReturnResultId(7))
                            .unwrap()
                        else {
                            unreachable!()
                        };
                        records[0].trigger_source_context = None;
                    }
                    _ => unreachable!(),
                }
                let error =
                    serde_json::from_value::<GameState>(serde_json::to_value(&hostile).unwrap())
                        .unwrap_err()
                        .to_string();
                if fault == 0 || fault == 4 {
                    assert!(
                        error.contains("instruction-result process owner mismatch"),
                        "{error}"
                    );
                }
                let mut reached = GameRunner::from_state(hostile);
                let before = serde_json::to_value(reached.state()).unwrap();
                let error = reached
                    .act(GameAction::ChooseManaColor {
                        choice: ManaChoice::SingleColor(ManaType::Green),
                        count: 1,
                    })
                    .unwrap_err()
                    .to_string();
                if fault == 0 || fault == 4 {
                    assert!(
                        error.contains("instruction-result process owner mismatch"),
                        "{error}"
                    );
                }
                assert_eq!(serde_json::to_value(reached.state()).unwrap(), before, "invalid response cannot mutate prompt, mana units, source, counter, zone or event state");
            }
            let mut intact = positive;
            let saved = serde_json::to_value(intact.state()).unwrap();
            *intact.state_mut() =
                serde_json::from_value(saved).expect("intact five process remains loadable");
            intact
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(ManaType::Green),
                    count: 1,
                })
                .unwrap();
            assert_departure_mana_completion(&intact, spell);
            let units: Vec<_> = intact.state().players[usize::from(shape == 6)]
                .mana_pool
                .units()
                .collect();
            assert_eq!(
                units.len(),
                if shape == 4 {
                    5
                } else if shape == 6 {
                    2
                } else {
                    1
                }
            );
            assert_eq!(units[0].color, ManaType::Green);
            assert!(units[0].source_could_produce_two_or_more_colors);
        }
    }

    #[test]
    fn departure_source_only_mana_output_stays_unavailable_despite_an_actual_ambient_result() {
        use crate::types::actions::GameAction;
        let (mut runner, spell, _, _) = departure_dynamic_mana_runner(4, false, false);
        let WaitingFor::ChooseManaColor {
            return_result_occurrence,
            context: ManaChoiceContext::ResolvingEffect(body),
            ..
        } = &runner.state().waiting_for
        else {
            unreachable!()
        };
        let occurrence = return_result_occurrence.unwrap();
        let body = body.clone();
        let Effect::Mana { produced, .. } = &body.effect else {
            unreachable!()
        };
        let produced = produced.clone();
        let previous = runner.state().active_return_result_occurrence;
        runner.state_mut().active_return_result_occurrence = Some(occurrence);
        assert!(resolve_mana_types(&produced, runner.state(), body.controller, body.source_id).is_empty(), "source-only output cannot authenticate the actual available R7 by borrowing ambient O/P");
        assert_eq!(
            resolve_mana_types_for_ability(&produced, runner.state(), &body).len(),
            5
        );
        let fixed = ManaProduction::Colorless {
            count: QuantityExpr::Fixed { value: 3 },
        };
        assert_eq!(
            resolve_mana_types(&fixed, runner.state(), body.controller, body.source_id),
            vec![ManaType::Colorless; 3]
        );
        let mut journal = produced;
        journal.for_each_quantity_expr_mut(&mut |quantity| {
            quantity.each_ref_mut(&mut |reference| {
                if let QuantityRef::ZoneChangeAggregate { source, .. } = reference {
                    *source = crate::types::ability::ZoneChangeRecordSource::ThisTurn;
                }
            })
        });
        assert_eq!(
            resolve_mana_types(&journal, runner.state(), body.controller, body.source_id).len(),
            5
        );
        runner.state_mut().active_return_result_occurrence = previous;
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Green),
                count: 1,
            })
            .unwrap();
        assert_departure_mana_completion(&runner, spell);
        assert_eq!(runner.state().players[0].mana_pool.total(), 5);
    }

    #[test]
    fn departure_final_mana_count_reads_five_but_latched_combination_keeps_its_units() {
        use crate::types::actions::GameAction;
        for shape in [4, 5] {
            let (mut runner, spell, _, _) = departure_dynamic_mana_runner(shape, false, false);
            let WaitingFor::ChooseManaColor {
                choice,
                return_result_occurrence,
                context: ManaChoiceContext::ResolvingEffect(ability),
                ..
            } = &runner.state().waiting_for
            else {
                panic!("actual final count prompt reached");
            };
            assert!(ability.sub_ability.is_none());
            assert!(ability.return_result_process.is_some());
            if shape == 4 {
                assert!(
                    matches!(choice, ManaChoicePrompt::SingleColor { options } if options == &[ManaType::Red, ManaType::Green])
                );
                assert!(return_result_occurrence.is_some());
                assert_eq!(runner.state().return_result_frames.len(), 1);
            } else {
                assert!(
                    matches!(choice, ManaChoicePrompt::AnyCombination { count: 5, options } if options == &[ManaType::Red, ManaType::Green])
                );
                assert!(
                    return_result_occurrence.is_none(),
                    "latched count alone does not retain an invocation"
                );
                assert!(runner.state().return_result_frames.is_empty());
            }
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved)
                .expect("current count and latched count use their own restore contract");
            runner
                .act(GameAction::ChooseManaColor {
                    choice: if shape == 4 {
                        ManaChoice::SingleColor(ManaType::Green)
                    } else {
                        ManaChoice::Combination(vec![ManaType::Green; 5])
                    },
                    count: 1,
                })
                .unwrap();
            assert_departure_mana_completion(&runner, spell);
            let units: Vec<_> = runner.state().players[0].mana_pool.units().collect();
            assert_eq!(units.len(), 5);
            assert!(units.iter().all(|unit| unit.color == ManaType::Green
                && unit.source_id == spell
                && unit.source_could_produce_two_or_more_colors));
            let (empty, empty_spell, _, _) = departure_dynamic_mana_runner(shape, false, true);
            assert_departure_mana_completion(&empty, empty_spell);
            assert_eq!(empty.state().players[0].mana_pool.total(), 0);
        }
    }

    #[test]
    fn departure_dynamic_color_view_and_count_view_keep_original_target_order() {
        use crate::game::scenario::{P0, P1};
        use crate::types::actions::GameAction;
        let (mut runner, spell, _, _) = departure_dynamic_mana_runner(6, false, false);
        let WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            return_result_occurrence: Some(occurrence),
            context: ManaChoiceContext::ResolvingEffect(ability),
        } = &runner.state().waiting_for
        else {
            panic!("Both-role dynamic prompt reached");
        };
        assert_eq!(*player, P1);
        assert_eq!(options, &[ManaType::Red, ManaType::Green]);
        assert_eq!(
            ability.targets,
            vec![TargetRef::Player(P1), TargetRef::Player(P0)]
        );
        assert_eq!(
            runner.state().players[1].hand.len(),
            1,
            "recipient hand population at the actual read"
        );
        assert_eq!(
            runner.state().players[0].hand.len(),
            2,
            "count-source hand excludes the paid resolving source"
        );
        let count_view = count_scoped_ability(
            runner.state(),
            ability,
            match &ability.effect {
                Effect::Mana { target, .. } => target.as_ref(),
                _ => unreachable!(),
            },
        );
        assert_eq!(count_view.targets, vec![TargetRef::Player(P0)]);
        assert_eq!(
            count_view.return_result_process,
            ability.return_result_process
        );
        assert_eq!(
            runner.state().return_result_frames[occurrence].process,
            ability.return_result_process
        );
        let colored: Vec<_> = runner
            .state()
            .battlefield
            .iter()
            .filter_map(|id| {
                let object = &runner.state().objects[id];
                (object.controller == P0 && !object.color.is_empty())
                    .then_some((object.effective_mana_value(), object.color.clone()))
            })
            .collect();
        assert!(colored.contains(&(6, vec![ManaColor::Red])));
        assert!(colored.contains(&(6, vec![ManaColor::Green])));
        assert!(
            colored.contains(&(7, vec![ManaColor::Blue])),
            "the count-view color bug has a distinct eligible decoy"
        );
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("Both operand views preserve one independent process authority");
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Green),
                count: 1,
            })
            .unwrap();
        assert_departure_mana_completion(&runner, spell);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        let units: Vec<_> = runner.state().players[1].mana_pool.units().collect();
        assert_eq!(units.len(), 2);
        assert!(units.iter().all(|unit| unit.color == ManaType::Green
            && unit.source_id == spell
            && unit.source_could_produce_two_or_more_colors));
    }

    #[test]
    fn departure_same_source_definition_and_result_can_complete_seven_then_paid_five_activation() {
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, AggregateFunction, ObjectProperty,
            ReturnResultId, ZoneChangeRecordSource,
        };
        use crate::types::actions::GameAction;
        use crate::types::mana::{ManaCost, ManaUnit};
        use crate::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature(P0, "Rumbling Baloth", 4, 4);
        scenario.add_creature(P0, "Centaur Courser", 3, 3);
        scenario.add_creature(P1, "Grizzly Bears", 2, 2);
        let second = [
            scenario
                .add_creature_to_hand(P0, "Centaur Courser", 3, 3)
                .id(),
            scenario
                .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
                .id(),
            scenario
                .add_creature_to_hand(P1, "Grizzly Bears", 2, 2)
                .id(),
        ];
        let amount = QuantityExpr::Ref {
            qty: QuantityRef::ZoneChangeAggregate {
                source: ZoneChangeRecordSource::InstructionResult {
                    result: ReturnResultId(7),
                },
                from: Some(Zone::Battlefield),
                to: Some(Zone::Exile),
                filter: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature()
                        .controller(ControllerRef::SpecificPlayer { id: P0 }),
                ),
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
            },
        };
        let mut effect: Effect = serde_json::from_value(serde_json::json!({"type":"ChangeZoneAll", "origin":"Battlefield", "destination":"Exile"})).unwrap();
        let Effect::ChangeZoneAll { target, .. } = &mut effect else {
            unreachable!()
        };
        *target = TargetFilter::Typed(crate::types::ability::TypedFilter::creature());
        let mut root = AbilityDefinition::new(AbilityKind::Activated, effect);
        root.cost = Some(AbilityCost::Mana {
            cost: ManaCost::Cost {
                generic: 1,
                shards: vec![],
            },
        });
        root.declares_return_result = Some(ReturnResultId(7));
        root.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: amount,
                    color_options: vec![ManaColor::Red, ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )));
        let source = scenario
            .add_artifact_from_oracle(P0, "Supplemental canonical reusable process", "")
            .with_ability_definition(root)
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
        );
        let mut runner = scenario.build();
        let original = runner.state().objects[&source].clone();
        let mut previous = None;
        for expected_count in [7, 5] {
            if expected_count == 5 {
                let mut setup_events = Vec::new();
                for card in second {
                    crate::game::zones::move_to_zone(
                        runner.state_mut(),
                        card,
                        Zone::Battlefield,
                        &mut setup_events,
                    );
                }
            }
            let before = runner.state().players[0].mana_pool.total();
            runner
                .act(GameAction::ActivateAbility {
                    payment_mode: crate::types::game_state::CastPaymentMode::Auto,
                    source_id: source,
                    ability_index: 0,
                })
                .unwrap();
            for _ in 0..8 {
                if matches!(
                    runner.state().waiting_for,
                    WaitingFor::ChooseManaColor { .. }
                ) {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            let WaitingFor::ChooseManaColor {
                return_result_occurrence: Some(occurrence),
                context: ManaChoiceContext::ResolvingEffect(ability),
                choice: ManaChoicePrompt::SingleColor { options },
                ..
            } = &runner.state().waiting_for
            else {
                panic!("each paid activation reaches its final public mana reader");
            };
            assert_eq!(options, &[ManaType::Red, ManaType::Green]);
            assert_eq!(ability.source_id, source);
            let process = ability.return_result_process.unwrap();
            assert!(
                runner.state().return_result_frames.contains_key(occurrence),
                "paid mana count={expected_count}: prompt owner={occurrence:?}/{process:?}; live={:?}; validation={:?}; frames={:?}; selected={:?}",
                crate::types::game_state::live_return_result_occurrences(runner.state()),
                validate_mana_choice_response(runner.state(), ability,
                    &ManaChoicePrompt::SingleColor { options: options.clone() }, Some(*occurrence)),
                runner.state().return_result_frames,
                runner.state().active_return_result_occurrence
            );
            assert_eq!(
                runner.state().return_result_frames[occurrence].process,
                Some(process)
            );
            if let Some(old) = previous {
                assert_ne!(old, process);
            }
            previous = Some(process);
            let Effect::Mana {
                produced: ManaProduction::AnyOneColor { count, .. },
                ..
            } = &ability.effect
            else {
                unreachable!()
            };
            let mut observed = runner.state().clone();
            let observed_count = super::super::with_inherited_return_result_occurrence(
                &mut observed,
                Some(*occurrence),
                |state| {
                    validate_mana_choice_response(
                        state,
                        ability,
                        &ManaChoicePrompt::SingleColor {
                            options: options.clone(),
                        },
                        Some(*occurrence),
                    )
                    .expect("the observed count uses the actual retained prompt owner");
                    resolve_quantity_with_targets(state, count, ability)
                },
            );
            assert_eq!(observed_count, expected_count);
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved)
                .expect("same-source activation keeps its new independent process");
            runner
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(ManaType::Green),
                    count: expected_count as u32,
                })
                .unwrap();
            assert_eq!(
                runner.state().players[0].mana_pool.total(),
                before - 1 + expected_count as usize
            );
            assert!(runner.state().return_result_frames.is_empty());
            assert!(runner.state().stack.is_empty());
            assert!(runner.state().resolution_stack.is_empty());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            let current = &runner.state().objects[&source];
            assert_eq!(current.zone, Zone::Battlefield);
            assert_eq!(current.incarnation, original.incarnation);
            assert_eq!(current.abilities, original.abilities);
            assert_eq!(current.base_abilities, original.base_abilities);
        }
    }

    #[test]
    fn departure_dynamic_mana_illegal_answer_retains_the_exact_prompt_and_owned_frame() {
        use crate::types::actions::GameAction;
        for shape in [0, 2, 4, 6] {
            let (mut runner, spell, _, _) = departure_dynamic_mana_runner(shape, false, false);
            for choice in [
                ManaChoice::SingleColor(ManaType::Blue),
                ManaChoice::Combination(vec![ManaType::Green]),
            ] {
                let before = serde_json::to_value(runner.state()).unwrap();
                assert!(runner
                    .act(GameAction::ChooseManaColor { choice, count: 1 })
                    .is_err());
                assert_eq!(serde_json::to_value(runner.state()).unwrap(), before, "invalid color or answer shape cannot consume the result authority or deposit mana");
            }
            runner
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(ManaType::Green),
                    count: 1,
                })
                .unwrap();
            assert_departure_mana_completion(&runner, spell);
            let recipient = usize::from(shape == 6);
            assert_eq!(
                runner.state().players[recipient].mana_pool.total(),
                if shape == 4 {
                    5
                } else if shape == 6 {
                    2
                } else {
                    1
                }
            );
        }
    }

    #[test]
    fn departure_independent_source_profile_opponent_aura_and_inner_land_views_cannot_borrow_ambient_results(
    ) {
        use crate::game::mana_sources;
        use crate::game::scenario::{P0, P1};
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, TriggerDefinition,
        };
        use crate::types::actions::GameAction;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::triggers::TriggerMode;
        let (mut runner, spell, _, _) = departure_dynamic_mana_runner(0, false, false);
        let WaitingFor::ChooseManaColor {
            return_result_occurrence: Some(occurrence),
            context: ManaChoiceContext::ResolvingEffect(actual),
            ..
        } = &runner.state().waiting_for
        else {
            unreachable!()
        };
        let actual = actual.clone();
        let Effect::Mana { produced, .. } = &actual.effect else {
            unreachable!()
        };
        let produced = produced.clone();
        let mut state = runner.state().clone();
        state.active_return_result_occurrence = Some(*occurrence);
        assert!(
            mana_sources::mana_production_could_produce_two_or_more_colors(
                &state,
                P0,
                spell,
                &produced,
                Some(&actual)
            )
        );
        assert!(!mana_sources::mana_production_could_produce_two_or_more_colors(&state, P0, spell, &produced, None), "an independent display/source-quality view never inherits a private result merely because O is ambient");
        let inner = crate::game::zones::create_object(
            &mut state,
            CardId(88001),
            P0,
            "Supplemental canonical independent inner land".into(),
            Zone::Battlefield,
        );
        let mut definition = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: produced.clone(),
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        );
        definition.cost = Some(AbilityCost::Tap);
        {
            let land = state.objects.get_mut(&inner).unwrap();
            land.card_types.core_types = vec![CoreType::Land];
            land.base_card_types = land.card_types.clone();
            land.abilities = std::sync::Arc::new(vec![definition.clone()]);
            land.base_abilities = land.abilities.clone();
        }
        let inner_filter = TargetFilter::SpecificObject { id: inner };
        assert!(mana_sources::produceable_mana_types_by_filter(&state, &inner_filter, P0, spell, Some(&actual)).is_empty(), "the bound outer land filter cannot lend its P to an independently surveyed land ability");
        assert!(mana_sources::activatable_land_mana_options(&state, inner, P0).is_empty());
        assert!(mana_sources::activatable_mana_options(&state, inner, P0).is_empty());
        assert!(!mana_sources::source_could_produce_two_or_more_colors(
            &state, inner, P0
        ));
        assert!(mana_sources::opponent_land_color_options(&state, P1).is_empty());
        let aura = crate::game::zones::create_object(
            &mut state,
            CardId(88002),
            P0,
            "Supplemental canonical independent mana Aura".into(),
            Zone::Battlefield,
        );
        {
            let object = state.objects.get_mut(&aura).unwrap();
            object.card_types.core_types = vec![CoreType::Enchantment];
            object.card_types.subtypes = vec!["Aura".into()];
            object.attached_to = Some(inner.into());
            object.push_printed_trigger(
                TriggerDefinition::new(TriggerMode::TapsForMana)
                    .valid_card(TargetFilter::AttachedTo)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    )),
            );
        }
        assert!(
            mana_sources::taps_for_mana_trigger_sources(&state).contains(&aura),
            "the dynamic Aura is an actual registered survey source"
        );
        assert!(
            mana_sources::taps_for_mana_aura_bonus(&state, inner, P0).is_empty(),
            "an independently surveyed mana-trigger definition cannot borrow the parent's result"
        );
        definition.effect = Box::new(Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        });
        {
            let land = state.objects.get_mut(&inner).unwrap();
            land.abilities = std::sync::Arc::new(vec![definition]);
            land.base_abilities = land.abilities.clone();
        }
        assert_eq!(
            mana_sources::produceable_mana_types_by_filter(
                &state,
                &inner_filter,
                P0,
                spell,
                Some(&actual)
            ),
            vec![ManaType::Green]
        );
        assert!(!mana_sources::activatable_land_mana_options(&state, inner, P0).is_empty());
        assert!(!mana_sources::activatable_mana_options(&state, inner, P0).is_empty());
        assert_eq!(
            mana_sources::opponent_land_color_options(&state, P1),
            vec![ManaColor::Green]
        );
        let ordinary_aura = crate::game::zones::create_object(
            &mut state,
            CardId(88003),
            P0,
            "Supplemental ordinary mana Aura twin".into(),
            Zone::Battlefield,
        );
        {
            let object = state.objects.get_mut(&ordinary_aura).unwrap();
            object.card_types.core_types = vec![CoreType::Enchantment];
            object.card_types.subtypes = vec!["Aura".into()];
            object.attached_to = Some(inner.into());
            object.push_printed_trigger(
                TriggerDefinition::new(TriggerMode::TapsForMana)
                    .valid_card(TargetFilter::AttachedTo)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::Fixed {
                                colors: vec![ManaColor::Red],
                                contribution: ManaContribution::Additional,
                            },
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    )),
            );
        }
        let bonus = mana_sources::taps_for_mana_aura_bonus(&state, inner, P0);
        assert_eq!(bonus.len(), 1);
        assert_eq!(bonus[0].1, vec![ManaType::Red]);
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Green),
                count: 1,
            })
            .unwrap();
        assert_departure_mana_completion(&runner, spell);
        assert_eq!(runner.state().players[0].mana_pool.total(), 1);
    }

    #[test]
    fn departure_generic_stored_waiting_slots_keep_only_the_selected_mana_owner() {
        use crate::game::scenario::{GameRunner, P0, P1};
        use crate::types::actions::GameAction;
        use crate::types::game_state::{
            FinalizePrePaymentChecks, PendingCostMoveCompletion, PendingCostMoveResume,
            PendingTriggerOrder, ResolutionPaymentTransaction,
        };
        // Supplemental typed custody fixtures wrap an ACTUALLY REACHED public mana
        // prompt. They test each existing saved WaitingFor slot, not a fictional
        // trigger or casting producer. Its real producer/payment/output are tested
        // before and after custody, with the exact suspended executable unchanged.
        for slot in 0..5 {
            let (positive, spell, _, _) = departure_dynamic_mana_runner(0, false, false);
            let WaitingFor::ChooseManaColor {
                return_result_occurrence: Some(occurrence),
                context: ManaChoiceContext::ResolvingEffect(body),
                ..
            } = &positive.state().waiting_for
            else {
                panic!("real dynamic color prompt reached");
            };
            let occurrence = *occurrence;
            let process = body.return_result_process.unwrap();
            let body = body.clone();
            let original_wait = positive.state().waiting_for.clone();
            let mut stored = positive.state().clone();
            stored.waiting_for = WaitingFor::Priority { player: P0 };
            match slot {
                0 => {
                    stored.pending_trigger_order = Some(PendingTriggerOrder {
                        groups: vec![],
                        resume_after_ordering: Some(Box::new(original_wait.clone())),
                    })
                }
                1 => {
                    stored.payment_transaction = Some(Box::new(ResolutionPaymentTransaction {
                        wire_version: 1,
                        owner: P0,
                        root: body.clone(),
                        transcript: vec![],
                        base_waiting_for: original_wait.clone(),
                        resolving_trigger_context:
                            crate::types::game_state::ResolvingTriggerContext::capture(&stored),
                    }))
                }
                2..=4 => {
                    let completion = if slot == 2 {
                        PendingCostMoveCompletion::FinalizeCast {
                            phyrexian_choices: None,
                            cascade_cast_transformed: false,
                            resolution_success_waiting_for: Some(Box::new(original_wait.clone())),
                            prepaid_actual_mana_spent: None,
                        }
                    } else {
                        PendingCostMoveCompletion::FinalizeDelvedCast {
                            phyrexian_choices: None,
                            pre_payment_checks: Some(Box::new(FinalizePrePaymentChecks {
                                early_waiting_for: (slot == 3).then(|| original_wait.clone()),
                                cascade_cast_transformed: false,
                                resolution_success_waiting_for: (slot == 4)
                                    .then(|| original_wait.clone()),
                                cast_this_way_etb_counter: None,
                                cast_this_way_enters_mods: Default::default(),
                            })),
                        }
                    };
                    stored.pending_cost_move_resume = Some(PendingCostMoveResume::Cast {
                        player: P0,
                        pending: None,
                        chosen: vec![],
                        paused_at_index: 0,
                        destination: Zone::Graveyard,
                        completion,
                        deferred_cost_events: vec![],
                        departure_record_indices: vec![],
                    });
                }
                _ => unreachable!(),
            }
            assert!(
                crate::types::game_state::live_return_result_occurrences(&stored)
                    .contains(&occurrence),
                "slot {slot} retains the actual response owner"
            );
            super::super::retire_unreferenced_return_result_frames(&mut stored);
            assert_eq!(
                stored.return_result_frames[&occurrence].process,
                Some(process)
            );
            crate::types::game_state::validate_return_result_occurrence_coherence(&stored)
                .expect("the exact saved prompt is coherent in every concrete generic slot");
            let wire = serde_json::to_value(&stored).unwrap();
            let restored: GameState = serde_json::from_value(wire)
                .expect("canonical GameState reload inspects the whole nested slot");
            for fault in 0..3 {
                let mut hostile = restored.clone();
                match fault {
                    0 => {
                        hostile
                            .return_result_frames
                            .get_mut(&occurrence)
                            .unwrap()
                            .process = Some(crate::types::game_state::ReturnResultProcessId(
                            process.0 + 100,
                        ))
                    }
                    1 => {
                        hostile.return_result_frames.remove(&occurrence);
                    }
                    2 => {
                        hostile
                            .return_result_frames
                            .get_mut(&occurrence)
                            .unwrap()
                            .results
                            .clear();
                    }
                    _ => unreachable!(),
                }
                hostile.next_return_result_process_id = process.0 + 101;
                assert!(!crate::types::game_state::live_return_result_occurrences(&hostile).contains(&occurrence),
                    "slot {slot}, fault {fault}: only actual agreeing authority retains a result frame");
                assert!(crate::types::game_state::validate_return_result_occurrence_coherence(&hostile).is_err(), "slot {slot}, fault {fault} cannot borrow or silently lose its unresolved reader");
                assert!(serde_json::from_value::<GameState>(
                    serde_json::to_value(hostile).unwrap()
                )
                .is_err());
            }
            for view in [
                crate::game::visibility::filter_state_for_viewer(&restored, P0),
                crate::game::visibility::filter_state_for_viewer(&restored, P1),
                crate::game::visibility::filter_state_for_unseated_viewer(&restored),
            ] {
                assert!(view.return_result_frames.is_empty());
                assert!(
                    !serde_json::to_string(&view)
                        .unwrap()
                        .contains("return_result_process"),
                    "slot {slot} is private for every audience"
                );
            }
            let mut resumed = restored;
            let wait = match slot {
                0 => *resumed
                    .pending_trigger_order
                    .take()
                    .unwrap()
                    .resume_after_ordering
                    .unwrap(),
                1 => resumed.payment_transaction.take().unwrap().base_waiting_for,
                _ => {
                    let PendingCostMoveResume::Cast { completion, .. } =
                        resumed.pending_cost_move_resume.take().unwrap()
                    else {
                        unreachable!()
                    };
                    match completion {
                        PendingCostMoveCompletion::FinalizeCast {
                            resolution_success_waiting_for: Some(wait),
                            ..
                        } => *wait,
                        PendingCostMoveCompletion::FinalizeDelvedCast {
                            pre_payment_checks: Some(checks),
                            ..
                        } => {
                            if slot == 3 {
                                checks.early_waiting_for.unwrap()
                            } else {
                                checks.resolution_success_waiting_for.unwrap()
                            }
                        }
                        _ => unreachable!(),
                    }
                }
            };
            assert_eq!(
                wait, original_wait,
                "custody cannot rewrite the actual bound executable or selected lookup"
            );
            resumed.waiting_for = wait;
            let mut runner = GameRunner::from_state(resumed);
            runner
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(ManaType::Green),
                    count: 1,
                })
                .unwrap();
            assert_eq!(runner.state().players[0].mana_pool.total(), 1);
            assert!(
                runner.state().players[0]
                    .mana_pool
                    .units()
                    .next()
                    .unwrap()
                    .source_could_produce_two_or_more_colors
            );
            assert_departure_mana_completion(&runner, spell);
            let mut abandoned = stored;
            abandoned.pending_trigger_order = None;
            abandoned.payment_transaction = None;
            abandoned.pending_cost_move_resume = None;
            super::super::retire_unreferenced_return_result_frames(&mut abandoned);
            assert!(
                abandoned.return_result_frames.is_empty(),
                "slot {slot} releases its last owner when its actual saved prompt is discarded"
            );
        }
    }

    #[test]
    fn departure_actual_player_and_sequential_mana_repauses_keep_five_then_two_until_the_last_response(
    ) {
        use crate::game::scenario::{P0, P1};
        use crate::types::actions::GameAction;
        for shape in [7, 8] {
            let (mut runner, spell, _, _) = departure_dynamic_mana_runner(shape, false, false);
            let mut selected = None;
            for (iteration, player, expected) in [(0, P0, 5), (1, P1, 2)] {
                let WaitingFor::ChooseManaColor {
                    player: actor,
                    choice: ManaChoicePrompt::SingleColor { options },
                    return_result_occurrence: Some(occurrence),
                    context: ManaChoiceContext::ResolvingEffect(body),
                } = &runner.state().waiting_for
                else {
                    panic!("actual per-player/next-Mana prompt {shape}/{iteration}")
                };
                assert_eq!(*actor, player);
                assert_eq!(options, &[ManaType::Red, ManaType::Green]);
                let (occurrence, process) = (*occurrence, body.return_result_process.unwrap());
                assert_eq!(
                    runner.state().return_result_frames[&occurrence].process,
                    Some(process)
                );
                if let Some(previous) = selected {
                    assert_eq!(previous, (occurrence, process));
                } else {
                    selected = Some((occurrence, process));
                }
                assert_eq!(runner.state().return_result_frames.len(), 1);
                assert_eq!(
                    runner.state().players[player.0 as usize].mana_pool.total(),
                    0
                );
                let mut wrong_owner = runner.state().clone();
                wrong_owner
                    .return_result_frames
                    .get_mut(&occurrence)
                    .unwrap()
                    .process = Some(crate::types::game_state::ReturnResultProcessId(
                    process.0 + 100,
                ));
                wrong_owner.next_return_result_process_id = process.0 + 101;
                let before = serde_json::to_value(&wrong_owner).unwrap();
                assert!(serde_json::from_value::<GameState>(before.clone()).is_err());
                let mut bad = crate::game::scenario::GameRunner::from_state(wrong_owner);
                let error = bad
                    .act(GameAction::ChooseManaColor {
                        choice: ManaChoice::SingleColor(ManaType::Green),
                        count: 1,
                    })
                    .unwrap_err();
                assert!(format!("{error:?}").contains("process owner mismatch"));
                assert_eq!(serde_json::to_value(bad.state()).unwrap(), before);
                let saved = serde_json::to_value(runner.state()).unwrap();
                *runner.state_mut() = serde_json::from_value(saved).expect("the real next-seat/next-instruction owner keeps the same checked result through reload");
                runner
                    .act(GameAction::ChooseManaColor {
                        choice: ManaChoice::SingleColor(ManaType::Green),
                        count: 1,
                    })
                    .unwrap();
                let units: Vec<_> = runner.state().players[player.0 as usize]
                    .mana_pool
                    .units()
                    .collect();
                assert_eq!(units.len(), expected);
                assert!(units.iter().all(|unit| unit.color == ManaType::Green
                    && unit.source_id == spell
                    && unit.source_could_produce_two_or_more_colors));
                if iteration == 0 {
                    assert!(
                        matches!(runner.state().waiting_for, WaitingFor::ChooseManaColor { player, return_result_occurrence: Some(current), .. } if player == P1 && current == occurrence)
                    );
                    assert!(runner.state().return_result_frames.contains_key(&occurrence), "the first completed public response cannot retire the next real prompt's owner");
                }
            }
            assert_departure_mana_completion(&runner, spell);
            assert_eq!(runner.state().players[0].mana_pool.total(), 5);
            assert_eq!(runner.state().players[1].mana_pool.total(), 2);
            let (empty, empty_spell, _, _) = departure_dynamic_mana_runner(shape, false, true);
            assert_departure_mana_completion(&empty, empty_spell);
            assert_eq!(empty.state().players[0].mana_pool.total(), 0);
            assert_eq!(empty.state().players[1].mana_pool.total(), 0);
        }
    }

    #[test]
    fn departure_fixed_combination_prompt_is_latched_and_legacy_no_read_response_needs_no_result_owner(
    ) {
        use crate::types::actions::GameAction;
        for legacy in [false, true] {
            let (mut runner, spell, _, _) = departure_dynamic_mana_runner(9, false, false);
            let WaitingFor::ChooseManaColor {
                player,
                return_result_occurrence,
                choice: ManaChoicePrompt::Combination { options },
                context: ManaChoiceContext::ResolvingEffect(body),
            } = &mut runner.state_mut().waiting_for
            else {
                panic!("the actual fixed combination prompt is reached")
            };
            assert_eq!(*player, PlayerId(0));
            assert!(return_result_occurrence.is_none());
            assert_eq!(
                options,
                &[
                    vec![ManaType::Red, ManaType::Red],
                    vec![ManaType::Green, ManaType::Green]
                ]
            );
            if legacy {
                body.bind_return_result_process(None);
            }
            assert!(
                runner.state().return_result_frames.is_empty(),
                "fixed latched combinations retain no fabricated result authority"
            );
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved).expect(
                "ordinary fixed combination works with absent new expectation/selector fields",
            );
            runner
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::Combination(vec![ManaType::Green, ManaType::Green]),
                    count: 1,
                })
                .unwrap();
            assert_departure_mana_completion(&runner, spell);
            let units: Vec<_> = runner.state().players[0].mana_pool.units().collect();
            assert_eq!(units.len(), 2);
            assert!(units
                .iter()
                .all(|unit| unit.color == ManaType::Green && unit.source_id == spell));
        }
    }

    #[test]
    fn departure_latched_any_combination_keeps_the_actual_live_recipient_filter_owner() {
        use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, AggregateFunction, Comparator, ObjectProperty,
            PlayerFilter, PlayerRelation, PlayerScope, QuantityExpr, ReturnResultId,
            ZoneChangeRecordSource,
        };
        use crate::types::actions::GameAction;
        use crate::types::game_state::{InstructionResult, RevealedInstructionResult};
        // Supplemental canonical direct-resolution fixture: the real complete
        // producer executes here; only the resolving recipient-role predicate
        // reads its result. The latched one-unit count supplies no authority.
        for (empty, public, eligible) in [
            (false, false, true),
            (true, false, true),
            (false, true, true),
            (true, true, true),
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (true, true, false),
        ] {
            let (mut foreign, foreign_spell, _, _) = departure_dynamic_mana_runner(4, true, false);
            let WaitingFor::ChooseManaColor {
                return_result_occurrence: Some(foreign_occurrence),
                ..
            } = foreign.state().waiting_for
            else {
                panic!("real foreign-seven public producer reached")
            };
            let foreign_frame = foreign.state().return_result_frames[&foreign_occurrence].clone();
            foreign
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(ManaType::Green),
                    count: 1,
                })
                .unwrap();
            assert_departure_mana_completion(&foreign, foreign_spell);
            assert_eq!(foreign.state().players[0].mana_pool.total(), 7);
            let mut scenario = GameScenario::new();
            scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
            if !empty {
                scenario.add_creature(P0, "Centaur Courser", 3, 3);
                scenario.add_creature(P0, "Grizzly Bears", 2, 2);
                scenario.add_creature(P1, "Grizzly Bears", 2, 2);
            }
            if !eligible {
                for _ in 0..if empty { 1 } else { 6 } {
                    scenario.add_card_to_hand(P1, "Candidate hand boundary");
                }
            }
            let reference = QuantityRef::ZoneChangeAggregate {
                source: ZoneChangeRecordSource::InstructionResult {
                    result: ReturnResultId(7),
                },
                from: Some(Zone::Battlefield),
                to: Some(Zone::Exile),
                filter: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::SpecificPlayer { id: P0 }),
                ),
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
            };
            let mana = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Mana {
                    produced: ManaProduction::AnyCombination {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![ManaColor::Red, ManaColor::Green],
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: Some(ManaTargetRole::Recipient {
                        recipient: TargetFilter::PlayerMatching {
                            player: Box::new(PlayerFilter::PlayerAttribute {
                                relation: PlayerRelation::Opponent,
                                // `attr` is the candidate's scalar; the general
                                // addressed quantity belongs in its threshold.
                                attr: Box::new(QuantityRef::HandSize {
                                    player: PlayerScope::ScopedPlayer,
                                }),
                                comparator: Comparator::LE,
                                value: Box::new(QuantityExpr::Ref { qty: reference }),
                            }),
                        },
                    }),
                },
            );
            let producer: Effect = serde_json::from_value(serde_json::json!({"type":"ChangeZoneAll", "origin":"Battlefield", "destination":"Exile", "target": TargetFilter::Typed(TypedFilter::creature())})).unwrap();
            let mut root = AbilityDefinition::new(
                if public {
                    AbilityKind::Activated
                } else {
                    AbilityKind::Spell
                },
                producer,
            );
            root.declares_return_result = Some(ReturnResultId(7));
            root.sub_ability = Some(Box::new(if public {
                root.cost = Some(crate::types::ability::AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        generic: 1,
                        shards: vec![],
                    },
                });
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChooseOneOf {
                        chooser: PlayerFilter::Opponent,
                        branches: vec![
                            AbilityDefinition::new(
                                AbilityKind::Spell,
                                Effect::GainLife {
                                    player: TargetFilter::Controller,
                                    amount: QuantityExpr::Fixed { value: 1 },
                                },
                            ),
                            mana,
                        ],
                        kind: crate::types::ability::ChooseOneOfKind::Ordinary,
                    },
                )
            } else {
                mana
            }));
            let source = if public {
                scenario
                    .add_artifact_from_oracle(P0, "Supplemental canonical role reader", "")
                    .with_ability_definition(root.clone())
                    .id()
            } else {
                scenario
                    .add_artifact_from_oracle(P0, "Supplemental canonical role reader", "")
                    .id()
            };
            if public {
                scenario.with_mana_pool(
                    P0,
                    vec![ManaUnit::new(ManaType::Black, source, false, vec![])],
                );
            }
            let mut runner = scenario.build();
            let original_source = runner.state().objects[&source].clone();
            if !public {
                assert!(
                    original_source.abilities.is_empty(),
                    "the direct source never owns the admitted root"
                );
            }
            let mut events = vec![];
            if public {
                events.extend(
                    runner
                        .act(GameAction::ActivateAbility {
                            source_id: source,
                            ability_index: 0,
                            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
                        })
                        .unwrap()
                        .events,
                );
                for _ in 0..8 {
                    if matches!(
                        runner.state().waiting_for,
                        WaitingFor::ChooseOneOfBranch { .. }
                    ) {
                        break;
                    }
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    events.extend(runner.act(GameAction::PassPriority).unwrap().events);
                }
                let WaitingFor::ChooseOneOfBranch {
                    player: P1,
                    return_result_occurrence: Some(occurrence),
                    expected_return_result_process: Some(process),
                    controller: P0,
                    source_id,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!("the paid public producer must reach P1's ordinary branch choice");
                };
                assert_eq!(*source_id, source);
                let frame = &runner.state().return_result_frames[occurrence];
                assert_eq!(frame.process, Some(*process));
                assert!(
                    matches!(&frame.results[&ReturnResultId(7)], InstructionResult::ZoneChanges(records) if records.len() == if empty { 0 } else { 3 })
                );
                assert_eq!(runner.state().players[0].mana_pool.total(), 0);
                events.extend(
                    runner
                        .act(GameAction::ChooseBranch { index: 1 })
                        .unwrap()
                        .events,
                );
            } else {
                let mut executable =
                    crate::game::ability_utils::build_resolved_from_def(&root, source, P0);
                executable.push_target(TargetRef::Player(P1));
                super::super::resolve_ability_chain(
                    runner.state_mut(),
                    &executable,
                    &mut events,
                    0,
                )
                .unwrap();
            }
            assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::ZoneChanged { record, .. } if record.from_zone == Some(Zone::Battlefield) && record.to_zone == Zone::Exile)).count(), if empty { 0 } else { 3 });
            if !eligible {
                assert_eq!(
                    runner.state().players[1].hand.len(),
                    if empty { 1 } else { 6 }
                );
                assert_eq!(runner.state().players[0].mana_pool.total(), 0);
                assert_eq!(runner.state().players[1].mana_pool.total(), 0);
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert!(
                    runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty()
                );
                assert!(runner.state().return_result_frames.is_empty());
                assert_eq!(
                    runner.state().objects[&source].incarnation,
                    original_source.incarnation
                );
                assert_eq!(runner.state().objects[&source].controller, P0);
                if !public {
                    assert!(runner.state().objects[&source].abilities.is_empty());
                }
                continue;
            }
            let WaitingFor::ChooseManaColor {
                player,
                choice: ManaChoicePrompt::AnyCombination { count, options },
                return_result_occurrence: Some(occurrence),
                context: ManaChoiceContext::ResolvingEffect(body),
            } = &runner.state().waiting_for
            else {
                panic!("the live role filter, not its fixed latched count, owns the real recipient prompt")
            };
            assert_eq!(*player, P1);
            assert_eq!(*count, 1);
            assert_eq!(options, &[ManaType::Red, ManaType::Green]);
            assert!(body.sub_ability.is_none());
            let occurrence = *occurrence;
            let admitted = runner.state().return_result_frames[&occurrence]
                .admitted_program
                .as_deref()
                .expect("the full actual root survives after its prefix has disappeared");
            assert_eq!(admitted.declares_return_result, Some(ReturnResultId(7)));
            assert!(matches!(admitted.effect, Effect::ChangeZoneAll { .. }));
            assert_eq!(admitted.source_id, source);
            assert_eq!(admitted.return_result_process, body.return_result_process);
            let (_, namespace) = crate::types::ability::classify_instruction_result_invocation(
                admitted,
                crate::types::ability::InstructionResultEntry::Independent,
                None,
            )
            .unwrap();
            assert_eq!(namespace.len(), 1);
            assert_eq!(
                namespace.get(&ReturnResultId(7)),
                Some(&Some((Zone::Battlefield, Zone::Exile)))
            );
            assert_eq!(
                runner.state().return_result_frames[&occurrence].process,
                body.return_result_process
            );
            assert!(
                matches!(&runner.state().return_result_frames[&occurrence].results[&ReturnResultId(7)], InstructionResult::ZoneChanges(records) if records.len() == if empty { 0 } else { 3 })
            );
            // Once the prompt has latched one unit, an unused typed count is
            // no longer an imminent read. The genuinely live recipient filter
            // still owns R7; liveness must use the response walk too.
            let WaitingFor::ChooseManaColor {
                context: ManaChoiceContext::ResolvingEffect(body),
                ..
            } = &mut runner.state_mut().waiting_for
            else {
                unreachable!()
            };
            let Effect::Mana {
                produced: ManaProduction::AnyCombination { count, .. },
                ..
            } = &mut body.effect
            else {
                unreachable!()
            };
            *count = QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeAggregate {
                    source: ZoneChangeRecordSource::InstructionResult {
                        result: ReturnResultId(99),
                    },
                    from: Some(Zone::Battlefield),
                    to: Some(Zone::Exile),
                    filter: TargetFilter::Any,
                    function: AggregateFunction::Sum,
                    property: ObjectProperty::Power,
                },
            };
            assert!(
                crate::types::game_state::live_return_result_occurrences(runner.state())
                    .contains(&occurrence),
                "an unconsumed latched count cannot hide the real live role-filter owner"
            );
            for fault in 0..6 {
                let mut hostile = runner.state().clone();
                match fault {
                    0 => {
                        let WaitingFor::ChooseManaColor {
                            context: ManaChoiceContext::ResolvingEffect(body),
                            ..
                        } = &mut hostile.waiting_for
                        else {
                            unreachable!()
                        };
                        body.bind_return_result_process(None);
                    }
                    1 => {
                        hostile
                            .return_result_frames
                            .get_mut(&occurrence)
                            .unwrap()
                            .results
                            .remove(&ReturnResultId(7));
                    }
                    2 => {
                        hostile
                            .return_result_frames
                            .get_mut(&occurrence)
                            .unwrap()
                            .results
                            .insert(
                                ReturnResultId(7),
                                InstructionResult::RevealedCards(RevealedInstructionResult {
                                    revealed_cards: vec![],
                                }),
                            );
                    }
                    3 => {
                        hostile.return_result_frames.remove(&occurrence);
                        hostile
                            .return_result_frames
                            .insert(foreign_occurrence, foreign_frame.clone());
                        hostile.next_return_result_occurrence_id = foreign_occurrence.0 + 1;
                        hostile.next_return_result_process_id =
                            foreign_frame.process.unwrap().0 + 1;
                        let WaitingFor::ChooseManaColor {
                            return_result_occurrence,
                            ..
                        } = &mut hostile.waiting_for
                        else {
                            unreachable!()
                        };
                        *return_result_occurrence = Some(foreign_occurrence);
                    }
                    4 => {
                        let WaitingFor::ChooseManaColor {
                            return_result_occurrence,
                            ..
                        } = &mut hostile.waiting_for
                        else {
                            unreachable!()
                        };
                        *return_result_occurrence = None;
                    }
                    5 => {
                        hostile.return_result_frames.remove(&occurrence);
                    }
                    _ => unreachable!(),
                }
                let before = serde_json::to_value(&hostile).unwrap();
                assert!(serde_json::from_value::<GameState>(before.clone()).is_err());
                let mut bad = GameRunner::from_state(hostile);
                let error = bad
                    .act(GameAction::ChooseManaColor {
                        choice: ManaChoice::Combination(vec![ManaType::Green]),
                        count: 1,
                    })
                    .unwrap_err();
                if fault == 3 {
                    assert!(format!("{error:?}").contains("process owner mismatch"), "the complete foreign frame fails independent expectation before any orphan fallback");
                }
                assert_eq!(serde_json::to_value(bad.state()).unwrap(), before, "fail before role revalidation, count, recipient deposit, events or choice mutation");
            }
            for fault in 0..10 {
                let mut hostile = runner.state().clone();
                match fault {
                    0 => {
                        hostile
                            .return_result_frames
                            .get_mut(&occurrence)
                            .unwrap()
                            .declarations
                            .as_mut()
                            .unwrap()
                            .insert(ReturnResultId(7), Some((Zone::Hand, Zone::Exile)));
                    }
                    1 => {
                        hostile
                            .return_result_frames
                            .get_mut(&occurrence)
                            .unwrap()
                            .declarations
                            .as_mut()
                            .unwrap()
                            .insert(ReturnResultId(99), None);
                    }
                    2 => {
                        hostile
                            .return_result_frames
                            .get_mut(&occurrence)
                            .unwrap()
                            .admitted_program
                            .as_mut()
                            .unwrap()
                            .source_id = ObjectId(u64::MAX);
                    }
                    3 => {
                        hostile
                            .return_result_frames
                            .get_mut(&occurrence)
                            .unwrap()
                            .admitted_program
                            .as_mut()
                            .unwrap()
                            .set_source_incarnation_recursive(Some(u64::MAX));
                    }
                    4..=8 => {
                        let WaitingFor::ChooseManaColor {
                            context: ManaChoiceContext::ResolvingEffect(body),
                            choice,
                            ..
                        } = &mut hostile.waiting_for
                        else {
                            unreachable!()
                        };
                        match fault {
                            4 => body.controller = P1,
                            5 => body.chosen_x = Some(77),
                            6 => {
                                let Effect::Mana { target, .. } = &mut body.effect else {
                                    unreachable!()
                                };
                                *target = Some(crate::types::ability::ManaTargetRole::Recipient {
                                    recipient: TargetFilter::Controller,
                                });
                            }
                            7 => {
                                let ManaChoicePrompt::AnyCombination { options, .. } = choice
                                else {
                                    unreachable!()
                                };
                                *options = vec![ManaType::Blue, ManaType::Green];
                            }
                            8 => {
                                let ManaChoicePrompt::AnyCombination { count, .. } = choice else {
                                    unreachable!()
                                };
                                *count = 2;
                            }
                            _ => unreachable!(),
                        }
                    }
                    9 => {
                        hostile
                            .return_result_frames
                            .get_mut(&occurrence)
                            .unwrap()
                            .admitted_program
                            .as_mut()
                            .unwrap()
                            .declares_return_result = None
                    }
                    _ => unreachable!(),
                }
                let before = serde_json::to_value(&hostile).unwrap();
                assert!(
                    serde_json::from_value::<GameState>(before.clone()).is_err(),
                    "program/input fault {fault}"
                );
                let mut bad = GameRunner::from_state(hostile);
                bad.act(GameAction::ChooseManaColor { choice: ManaChoice::Combination(vec![ManaType::Green]), count: 1 })
                    .expect_err("an altered namespace, operation or invocation input is rejected before deposit");
                assert_eq!(
                    serde_json::to_value(bad.state()).unwrap(),
                    before,
                    "program/input fault {fault} is transactional"
                );
            }
            let mut missing_map = runner.state().clone();
            missing_map
                .return_result_frames
                .get_mut(&occurrence)
                .unwrap()
                .declarations = None;
            let missing_map = serde_json::to_value(&missing_map).unwrap();
            let restored: GameState = serde_json::from_value(missing_map)
                .expect("the actual full program recovers an absent optional declaration map");
            assert_eq!(
                restored.return_result_frames[&occurrence]
                    .declarations
                    .as_ref()
                    .unwrap()
                    .len(),
                1
            );
            let mut restored = GameRunner::from_state(restored);
            restored
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::Combination(vec![ManaType::Green]),
                    count: 1,
                })
                .unwrap();
            assert_eq!(restored.state().players[0].mana_pool.total(), 0);
            assert_eq!(restored.state().players[1].mana_pool.total(), 1);
            assert!(
                restored.state().stack.is_empty()
                    && restored.state().resolution_stack.is_empty()
                    && restored.state().return_result_frames.is_empty()
            );
            if !public {
                let mut erased_origin = runner.state().clone();
                erased_origin
                    .return_result_frames
                    .get_mut(&occurrence)
                    .unwrap()
                    .admitted_program = None;
                assert!(erased_origin.resolving_stack_entry.is_none());
                assert!(erased_origin.objects[&source].abilities.is_empty());
                let saved = serde_json::to_value(&erased_origin).unwrap();
                let error = serde_json::from_value::<GameState>(saved.clone())
                    .unwrap_err()
                    .to_string();
                assert!(
                    error.contains("unrecoverable full program origin"),
                    "{error}"
                );
                assert_eq!(serde_json::to_value(&erased_origin).unwrap(), saved);
            } else {
                let mut recoverable = runner.state().clone();
                recoverable
                    .return_result_frames
                    .get_mut(&occurrence)
                    .unwrap()
                    .admitted_program = None;
                let saved = serde_json::to_string(&recoverable).unwrap();
                let recovered: GameState = serde_json::from_str(&saved)
                    .expect("the actual public invocation retains recoverable full-program origin");
                assert!(recovered.return_result_frames[&occurrence]
                    .admitted_program
                    .is_some());
                let mut recovered = GameRunner::from_state(recovered);
                recovered
                    .act(GameAction::ChooseManaColor {
                        choice: ManaChoice::Combination(vec![ManaType::Green]),
                        count: 1,
                    })
                    .unwrap();
                assert_eq!(recovered.state().players[0].mana_pool.total(), 0);
                assert_eq!(recovered.state().players[1].mana_pool.total(), 1);
                assert!(
                    recovered.state().stack.is_empty()
                        && recovered.state().resolution_stack.is_empty()
                        && recovered.state().return_result_frames.is_empty()
                );
            }
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved)
                .expect("latched production still retains its genuinely live role-filter read");
            runner
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::Combination(vec![ManaType::Green]),
                    count: 1,
                })
                .unwrap();
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            let units: Vec<_> = runner.state().players[1].mana_pool.units().collect();
            assert_eq!(units.len(), 1);
            assert_eq!(units[0].color, ManaType::Green);
            assert_eq!(units[0].source_id, source);
            assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
            assert_eq!(
                runner.state().objects[&source].controller,
                original_source.controller
            );
            assert_eq!(
                runner.state().objects[&source].incarnation,
                original_source.incarnation
            );
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
            assert!(runner.state().return_result_frames.is_empty());
            if !public {
                assert!(
                    runner.state().objects[&source].abilities.is_empty(),
                    "no installed program was fabricated to repair detached origin"
                );
            }
        }
    }

    #[test]
    fn departure_independent_mana_activation_and_fixed_point_reconstruction_never_borrow_the_parent_prompt(
    ) {
        use crate::game::scenario::{GameRunner, P0};
        use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind, QuantityExpr};
        use crate::types::actions::GameAction;
        use crate::types::game_state::{
            ManaAbilityResume, ManaColorChoiceResume, ManaTriggerFixedPointResume,
        };
        let (mut runner, spell, _, _) = departure_dynamic_mana_runner(4, false, false);
        let parent_wait = runner.state().waiting_for.clone();
        let WaitingFor::ChooseManaColor {
            return_result_occurrence: Some(parent_occurrence),
            context: ManaChoiceContext::ResolvingEffect(parent_body),
            ..
        } = &parent_wait
        else {
            unreachable!()
        };
        let parent_occurrence = *parent_occurrence;
        let independent = crate::game::zones::create_object(
            runner.state_mut(),
            crate::types::identifiers::CardId(999_037),
            P0,
            "Supplemental canonical independent fixed-point source".into(),
            Zone::Battlefield,
        );
        let definition = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Red, ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        let object = runner.state_mut().objects.get_mut(&independent).unwrap();
        object
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);
        std::sync::Arc::make_mut(&mut object.abilities).push(definition.clone());
        // Typed source-boundary fixture: execute the existing independent
        // activation while a real five-result selector is ambient. This is not
        // a claim the parent effect's color prompt permits activating a source.
        runner.state_mut().active_return_result_occurrence = Some(parent_occurrence);
        let mut events = vec![];
        let waiting = crate::game::mana_abilities::activate_mana_ability(
            runner.state_mut(),
            independent,
            P0,
            0,
            &definition,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();
        let WaitingFor::ChooseManaColor {
            player,
            choice,
            context: context @ ManaChoiceContext::ManaAbility(_),
            return_result_occurrence: None,
        } = waiting
        else {
            panic!("the actual independent activation builds an unowned prompt")
        };
        assert_eq!(player, P0);
        assert!(
            matches!(&choice, ManaChoicePrompt::SingleColor { options } if options == &[ManaType::Red, ManaType::Green])
        );
        assert!(runner.state().objects[&independent].tapped);
        let outer = ManaTriggerFixedPointResume::ColorChoice(Box::new(ManaColorChoiceResume {
            player,
            choice: choice.clone(),
            context: context.clone(),
        }));
        let outer: ManaTriggerFixedPointResume =
            serde_json::from_value(serde_json::to_value(outer).unwrap()).unwrap();
        let reconstructed = crate::game::mana_abilities::resume_settled_mana_frame(
            runner.state_mut(),
            outer,
            &mut events,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            &reconstructed,
            WaitingFor::ChooseManaColor {
                return_result_occurrence: None,
                context: ManaChoiceContext::ManaAbility(_),
                ..
            }
        ));
        let forged = ManaTriggerFixedPointResume::ColorChoice(Box::new(ManaColorChoiceResume {
            player,
            choice,
            context: ManaChoiceContext::ResolvingEffect(parent_body.clone()),
        }));
        let forged_wait = crate::game::mana_abilities::resume_settled_mana_frame(
            runner.state_mut(),
            forged,
            &mut events,
        )
        .unwrap()
        .unwrap();
        let mut hostile = runner.state().clone();
        hostile.waiting_for = forged_wait;
        let before = serde_json::to_value(&hostile).unwrap();
        assert!(serde_json::from_value::<GameState>(before.clone()).is_err(), "a forged resolving import in an independent fixed-point continuation cannot obtain the parent selector");
        let mut bad = GameRunner::from_state(hostile);
        assert!(bad
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Green),
                count: 1
            })
            .is_err());
        assert_eq!(serde_json::to_value(bad.state()).unwrap(), before);
        runner.state_mut().waiting_for = reconstructed;
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).expect(
            "independent reconstructed prompt keeps None even beside the real ambient parent frame",
        );
        let WaitingFor::ChooseManaColor {
            choice,
            context: ManaChoiceContext::ManaAbility(pending),
            ..
        } = runner.state().waiting_for.clone()
        else {
            unreachable!()
        };
        let resume = crate::game::mana_abilities::handle_choose_mana_color(
            runner.state_mut(),
            &pending,
            &choice,
            ManaChoice::SingleColor(ManaType::Green),
            &mut events,
        )
        .unwrap();
        assert!(matches!(resume, WaitingFor::Priority { .. }));
        assert_eq!(runner.state().players[0].mana_pool.total(), 1);
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .units()
                .next()
                .unwrap()
                .source_id,
            independent
        );
        // Restore the exact actual parent owner from the typed fixture's custody;
        // its public response still reads five and retires only its own result.
        runner.state_mut().active_return_result_occurrence = None;
        runner.state_mut().waiting_for = parent_wait;
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).unwrap();
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Green),
                count: 1,
            })
            .unwrap();
        assert_eq!(runner.state().players[0].mana_pool.total(), 6);
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .units()
                .filter(|unit| unit.source_id == spell)
                .count(),
            5
        );
        assert_departure_mana_completion(&runner, spell);
    }
}
