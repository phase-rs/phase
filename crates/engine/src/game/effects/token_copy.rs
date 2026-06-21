use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::game_object::{DisplaySource, GameObject};
use crate::game::layers::{compute_current_copiable_values, has_active_copy_layer_effects};
use crate::game::printed_cards::intrinsic_copiable_values;
use crate::game::quantity::resolve_quantity;
use crate::game::{targeting, zones};
use crate::types::ability::{
    ContinuousModification, Effect, EffectError, EffectKind, ResolvedAbility, StaticDefinition,
    TargetFilter, TargetRef, TriggerCondition, TriggerDefinition,
};
use crate::types::card_type::SubtypeSet;
#[cfg(test)]
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, PendingCopyTokenBatch, PendingCopyTokenResolution, PendingCounterPostAction,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::proposed_event::{
    CopyTokenSpec, EtbTapState, ProposedEvent, TokenCharacteristics,
};
use crate::types::zones::Zone;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;

/// CR 707.2 / CR 707.5: Create a token that's a copy of a permanent.
/// Copies copiable characteristics from the target to a newly created token.
///
/// CR 707.2 + CR 614.1a: When `count` resolves to N > 1 (e.g. Rite of
/// Replication kicked = 5), N independent copy-tokens are created. The
/// per-source count is additionally routed through the `CreateToken`
/// replacement pipeline so token-count-doubling replacements (Doubling Season,
/// Adrix and Nev, Parallel Lives, Anointed Procession, Mondrak) apply uniformly
/// to copy-token creation, exactly as they do to predefined `Effect::Token`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Extract fields from effect
    let (
        target_filter,
        owner_filter,
        source_filter,
        enters_attacking,
        tapped,
        count_expr,
        extra_keywords,
        additional_modifications,
    ) = match &ability.effect {
        Effect::CopyTokenOf {
            target,
            owner,
            source_filter,
            enters_attacking,
            tapped,
            count,
            extra_keywords,
            additional_modifications,
        } => (
            target,
            owner,
            source_filter,
            *enters_attacking,
            *tapped,
            count.clone(),
            extra_keywords.clone(),
            additional_modifications.clone(),
        ),
        _ => return Err(EffectError::MissingParam("CopyTokenOf".to_string())),
    };
    let count = resolve_quantity(state, &count_expr, ability.controller, ability.source_id).max(0);

    // CR 109.4 + CR 111.2: The token's creator (and therefore controller) is
    // determined by the `owner` filter. Resolved once, before the creation
    // loops, through the same single-authority helper `Effect::Token` uses so
    // "target opponent creates a token that's a copy of it" places the copy
    // under the chosen opponent's control rather than the trigger controller's.
    let token_owner =
        crate::game::effects::token::resolve_token_owner(state, ability, owner_filter);

    // Step 1: Resolve the copy source list.
    // CR 608.2c + 603.10a: LTB self-trigger patterns such as Vaultborn Tyrant
    // ("create a token that's a copy of it") and Ochre Jelly's delayed trigger
    // emit `target: ParentTarget` / `SelfRef` with empty `ability.targets`.
    // In a top-level trigger there is no parent chain, so the anaphor refers to
    // the source object itself. `TriggeringSource` is deliberately excluded:
    // it resolves via `state.current_trigger_event`, not `source_id`.
    //
    // CR 115.1d + CR 601.2c: For "any number of target X" / "for each of them,
    // create a token …" (e.g., Twinflame), `ability.targets` carries N >= 1
    // object refs and the resolver creates one copy per target.
    //
    // Zone-eligibility: unlike `Bounce` / `ChangeZone`, `CopyTokenOf` reads
    // copiable values via `compute_current_copiable_values`, which is
    // zone-agnostic — so a source in the graveyard is fine.
    let copy_source_ids: Vec<ObjectId> = if let Some(source_filter) = source_filter {
        let zones = {
            let explicit_zones = source_filter.extract_zones();
            if explicit_zones.is_empty() {
                vec![Zone::Battlefield]
            } else {
                explicit_zones
            }
        };
        let filter_ctx = FilterContext::from_ability(ability);
        zones
            .into_iter()
            .flat_map(|zone| targeting::zone_object_ids(state, zone))
            .filter(|id| matches_target_filter(state, *id, source_filter, &filter_ctx))
            .collect()
    } else if matches!(target_filter, TargetFilter::CostPaidObject) {
        ability
            .cost_paid_object
            .as_ref()
            .map(|snapshot| vec![snapshot.object_id])
            .ok_or_else(|| {
                EffectError::MissingParam("CopyTokenOf requires a cost-paid object".to_string())
            })?
    } else if matches!(
        target_filter,
        TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. }
    ) {
        let effective_filter =
            crate::game::targeting::resolve_tracked_set_sentinel(state, target_filter.clone());
        let id = match &effective_filter {
            TargetFilter::TrackedSet { id } | TargetFilter::TrackedSetFiltered { id, .. } => *id,
            _ => unreachable!("tracked-set filter resolved to non-tracked filter"),
        };
        let filter_ctx = FilterContext::from_ability(ability);
        state
            .tracked_object_sets
            .get(&id)
            .into_iter()
            .flatten()
            .copied()
            .filter(|id| matches_target_filter(state, *id, &effective_filter, &filter_ctx))
            .collect()
    } else {
        // CR 608.2c + 603.10a: Delegate to the unified 3-tier dispatch so
        // `SelfRef` always resolves to the source object (the LTB
        // self-trigger shape — Vaultborn Tyrant, Ochre Jelly), and
        // `None` / `ParentTarget` fall back to source only when
        // `ability.targets` is empty. Without this, a chained
        // `CopyTokenOf { target: SelfRef }` sub-ability would inherit the
        // parent's targets via chain propagation in
        // `effects::mod.rs::resolve_ability_chain` (issue #323 class).
        //
        // CR 109.4 + CR 115.1: `CopyTokenOf` may carry a *player* target in
        // `ability.targets` — the `owner` slot for "target opponent creates a
        // token that's a copy of it" (Wedding Ring). The copy *source* axis is
        // object-only, so a context-ref source (`ParentTarget` / `None`) would
        // otherwise see the owner player as a non-empty `ability.targets` and
        // fail to fall back to the source object. Resolve against an
        // object-only view so the two axes never cross-contaminate.
        let object_only_ability;
        let resolution_ability = if ability
            .targets
            .iter()
            .any(|t| matches!(t, TargetRef::Player(_)))
        {
            let mut narrowed = ability.clone();
            narrowed
                .targets
                .retain(|t| matches!(t, TargetRef::Object(_)));
            object_only_ability = narrowed;
            &object_only_ability
        } else {
            ability
        };
        let effective_targets =
            crate::game::targeting::resolved_targets(resolution_ability, target_filter, state);
        crate::game::effects::effect_object_targets(target_filter, &effective_targets)
    };

    // CR 609.3 + CR 101.3: "Do as much as possible" — when the copy source
    // resolves empty, `CopyTokenOf` is a clean zero-token no-op rather than an
    // error. This is required for an unattached Springheart Nantuko: its
    // `target: AttachedTo` host resolves empty when the card is not bestowed
    // onto a creature, so the copy makes nothing and the chained
    // `Not(IfYouDo)` Insect-token fallback can still fire. `EffectResolved` is
    // still emitted so the chain treats the effect as resolved.
    if copy_source_ids.is_empty() {
        // No tokens created — clear the per-resolution token-id ledger so a
        // downstream "the token created this way" anaphor does not pick up a
        // stale id from an earlier resolution. Engine bookkeeping, not a
        // CR-specified rule.
        state.last_created_token_ids = Vec::new();
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 707.2 + CR 115.1d: Create `count` independent copy-tokens per copy
    // source. Snapshot all source values before the first creation so later SBAs
    // (e.g., legendary rule) see identical copies. The drain can pause and resume
    // when the `CreateToken` replacement pipeline requires a CR 616.1 choice.
    let mut remaining = VecDeque::with_capacity(copy_source_ids.len());
    for &copy_source_id in &copy_source_ids {
        let values = compute_current_copiable_values(state, copy_source_id)
            .ok_or(EffectError::ObjectNotFound(copy_source_id))?;
        let source = &state.objects[&copy_source_id];
        remaining.push_back(PendingCopyTokenBatch {
            owner: token_owner,
            count: count as u32,
            copy: Box::new(CopyTokenSpec {
                values: Box::new(values),
                display_source: source.display_source,
                printed_ref: source.printed_ref.clone(),
                token_image_ref: source.token_image_ref.clone(),
                extra_keywords: extra_keywords.clone(),
                additional_modifications: additional_modifications.clone(),
                tapped,
                enters_attacking,
                sacrifice_at: ability.duration.clone(),
                source_id: ability.source_id,
                controller: ability.controller,
            }),
        });
    }

    drive_copy_token_batches(
        state,
        remaining,
        EffectKind::from(&ability.effect),
        ability.source_id,
        events,
    );

    Ok(())
}

/// CR 707.2 + CR 614.1a: Route a queue of `PendingCopyTokenBatch`es through the
/// `CreateToken` replacement pipeline and apply path. Single authority shared by
/// `CopyTokenOf` (battlefield-sourced copies) and `CreateTokenCopyFromPool`
/// (format-pool-sourced copies) so the replacement + apply path is never
/// duplicated. The drain can pause and resume when a CR 616.1 replacement choice
/// is required.
pub(crate) fn drive_copy_token_batches(
    state: &mut GameState,
    remaining: VecDeque<PendingCopyTokenBatch>,
    effect_kind: EffectKind,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    drain_copy_token_resolution(
        state,
        PendingCopyTokenResolution {
            created_ids: Vec::new(),
            remaining,
            effect_kind,
            source_id,
        },
        events,
    );
}

pub(crate) fn drain_pending_copy_token_resolution(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) {
    if let Some(pending) = state.pending_copy_token_resolution.take() {
        drain_copy_token_resolution(state, pending, events);
    }
}

fn drain_copy_token_resolution(
    state: &mut GameState,
    mut pending: PendingCopyTokenResolution,
    events: &mut Vec<GameEvent>,
) {
    while let Some(batch) = pending.remaining.pop_front() {
        if batch.count == 0 {
            continue;
        }
        let spec = super::token::copy_probe_spec_for(
            batch.copy.source_id,
            batch.copy.controller,
            batch.copy.sacrifice_at.clone(),
            &batch.copy.values,
        );
        let mut spec = spec;
        spec.tapped = batch.copy.tapped;
        spec.enters_attacking = batch.copy.enters_attacking;
        let enter_tapped = EtbTapState::from_seeded_tapped(batch.copy.tapped);
        let proposed = ProposedEvent::CreateToken {
            owner: batch.owner,
            spec: Box::new(spec),
            copy: Some(batch.copy),
            enter_tapped,
            count: batch.count,
            applied: HashSet::new(),
        };

        match crate::game::replacement::replace_event(state, proposed, events) {
            crate::game::replacement::ReplacementResult::Execute(event) => {
                if !super::token::apply_create_token_after_replacement(state, event, events) {
                    pending
                        .created_ids
                        .extend(state.last_created_token_ids.clone());
                    state.pending_copy_token_resolution = Some(pending);
                    return;
                }
                pending
                    .created_ids
                    .extend(state.last_created_token_ids.clone());
            }
            crate::game::replacement::ReplacementResult::Prevented => {}
            crate::game::replacement::ReplacementResult::NeedsChoice(player) => {
                state.pending_copy_token_resolution = Some(pending);
                state.waiting_for =
                    crate::game::replacement::replacement_choice_waiting_for(player, state);
                return;
            }
        }
    }

    // CR 603.7 + CR 701.36a: Record created token IDs so sub-abilities can
    // reference them via `TargetFilter::LastCreated` ("the token created this
    // way", "it") and so "those tokens" plural anaphor in delayed triggers
    // captures the full list. Mirrors `token::apply_create_token`.
    state.last_created_token_ids = pending.created_ids;

    events.push(GameEvent::EffectResolved {
        kind: pending.effect_kind,
        source_id: pending.source_id,
    });
}

/// CR 601.2f + CR 603.4: Triggers gated on a cast-time additional-cost payment
/// ("if its offspring cost was paid", "if it was kicked") are inert on a token
/// copy — the token was created, not cast, so the payment condition can never
/// hold. Only the payment-gated condition is stripped: persistent battlefield
/// abilities that *observe* spell casts (Magecraft's `SpellCastOrCopy`,
/// "whenever you cast a [type] spell" `SpellCast`) are copiable text per
/// CR 707.2 and must survive on the copy.
fn is_cast_payment_gated_trigger(trig: &TriggerDefinition) -> bool {
    matches!(
        trig.condition,
        Some(TriggerCondition::AdditionalCostPaid { .. })
    )
}

/// CR 601.2f + CR 707.2: Remove spell-casting-only copiable characteristics
/// from a freshly created copy token (Offspring/Kicker keywords, "if it was
/// kicked" / "if offspring was paid" ETB triggers, etc.).
fn strip_spell_casting_copiable_characteristics(obj: &mut GameObject) {
    obj.keywords.retain(|kw| !kw.is_spell_casting_only());
    obj.base_keywords.retain(|kw| !kw.is_spell_casting_only());
    obj.trigger_definitions
        .retain(|trig| !is_cast_payment_gated_trigger(trig));
    Arc::make_mut(&mut obj.base_trigger_definitions)
        .retain(|trig| !is_cast_payment_gated_trigger(trig));
}

/// CR 111.10 + CR 702.175a: When a card-related token preset exists (Offspring,
/// set-specific copy tokens), route the copy through the token art database
/// instead of rendering as a card-copy with a "Copy" badge.
fn resolve_predefined_token_display(
    state: &mut GameState,
    copy_source_id: ObjectId,
    token_id: ObjectId,
) {
    let body = {
        let token = match state.objects.get(&token_id) {
            Some(token) if token.display_source == DisplaySource::Card => token,
            _ => return,
        };
        TokenCharacteristics {
            display_name: token.name.clone(),
            power: token.power,
            toughness: token.toughness,
            core_types: token.card_types.core_types.clone(),
            subtypes: token.card_types.subtypes.clone(),
            supertypes: token.card_types.supertypes.clone(),
            colors: token.color.clone(),
            keywords: token.keywords.clone(),
        }
    };
    let Some(image_ref) =
        crate::game::token_presets::find_card_linked_copy_token_ref(state, copy_source_id, &body)
    else {
        return;
    };
    if let Some(token) = state.objects.get_mut(&token_id) {
        token.display_source = DisplaySource::Token;
        token.token_image_ref = Some(image_ref);
    }
}

/// CR 707.2: Finalize a copy token after P/T exceptions and cast-only stripping.
fn finalize_copied_token(state: &mut GameState, copy_source_id: ObjectId, token_id: ObjectId) {
    if let Some(token) = state.objects.get_mut(&token_id) {
        strip_spell_casting_copiable_characteristics(token);
    }
    resolve_predefined_token_display(state, copy_source_id, token_id);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_copy_token_after_replacement(
    state: &mut GameState,
    token_owner: crate::types::player::PlayerId,
    copy: CopyTokenSpec,
    enter_tapped: EtbTapState,
    enter_with_counters: Vec<(crate::types::counter::CounterType, u32)>,
    final_count: u32,
    events: &mut Vec<GameEvent>,
) -> CopyTokenApplyStatus {
    let CopyTokenSpec {
        values,
        display_source,
        printed_ref,
        token_image_ref,
        extra_keywords,
        additional_modifications,
        tapped,
        enters_attacking,
        sacrifice_at,
        source_id,
        controller,
    } = copy;
    let name = values.name.clone();
    let mut created_ids = Vec::with_capacity(final_count as usize);
    let copied_loyalty =
        copy_starting_loyalty_override(&additional_modifications).or(values.loyalty);

    // CR 306.5b + CR 707.2 + CR 707.9b: A token that's a copy of a planeswalker
    // enters with loyalty counters equal to the copied loyalty, except a copy
    // exception may set that starting loyalty to a different value. CR 306.5c
    // makes the counter map the single source of truth for loyalty. The copy
    // path builds the object directly (not through the ZoneChange ETB-counter
    // seeding used by cast/play/effect entries), so seed the intrinsic loyalty
    // counters here, ahead of any explicit `enter_with_counters`, routing them
    // through `add_counter_with_replacement` below so Doubling Season etc. apply
    // (CR 614.1a). Without this a copied planeswalker enters with 0 loyalty
    // counters and dies immediately to CR 704.5i. Copies don't track battle
    // defense (`CopiableValues` has no defense field), so only loyalty is seeded.
    // CR 306.5b loyalty + CR 614.1c "~ enters with N counters" self-replacement
    // (Atraxa's Skitterfang's three oil counters, Hangarback/Walking Ballista
    // +1/+1, etc.) + any explicit counters the creating effect added. The copy
    // path bypasses the ZoneChange replacement pass, so the copied card's
    // intrinsic "enters with counters" replacement is seeded here from its
    // copiable replacement set rather than firing during entry.
    let etb_counters: Vec<(crate::types::counter::CounterType, u32)> =
        crate::game::printed_cards::intrinsic_face_counters(copied_loyalty, None)
            .into_iter()
            .chain(crate::game::printed_cards::self_etb_counter_replacements(
                &values.replacement_definitions,
            ))
            .chain(enter_with_counters.iter().cloned())
            .collect();

    for index in 0..final_count {
        let token_id = zones::create_object(
            state,
            CardId(0),
            token_owner,
            name.clone(),
            Zone::Battlefield,
        );

        let token = state.objects.get_mut(&token_id).unwrap();
        token.is_token = true;
        token.display_source = display_source;
        token.printed_ref = printed_ref.clone();
        token.base_printed_ref = printed_ref.clone();
        // CR 111.1 + CR 707.2: when copying a true token, carry its exact token
        // art pointer so the copy resolves the same art (not a name fallback).
        token.token_image_ref = token_image_ref.clone();
        token.name = values.name.clone();
        token.base_name = values.name.clone();
        token.mana_cost = values.mana_cost.clone();
        token.base_mana_cost = values.mana_cost.clone();
        token.base_color = values.color.clone();
        token.color = values.color.clone();
        token.base_card_types = values.card_types.clone();
        token.card_types = values.card_types.clone();
        token.base_power = values.power;
        token.power = values.power;
        token.base_toughness = values.toughness;
        token.toughness = values.toughness;
        token.base_loyalty = copied_loyalty;
        token.loyalty = copied_loyalty;
        token.base_keywords = values.keywords.clone();
        token.keywords = values.keywords.clone();
        // All four ability sets are Arc-shared — refcount bumps, no deep copy.
        token.base_abilities = Arc::clone(&values.abilities);
        token.abilities = Arc::clone(&values.abilities);
        token.base_trigger_definitions = Arc::clone(&values.trigger_definitions);
        token.trigger_definitions = Arc::clone(&values.trigger_definitions).into();
        token.base_replacement_definitions = Arc::clone(&values.replacement_definitions);
        token.replacement_definitions = Arc::clone(&values.replacement_definitions).into();
        token.base_static_definitions = Arc::clone(&values.static_definitions);
        token.static_definitions = Arc::clone(&values.static_definitions).into();
        token.base_characteristics_initialized = true;
        // CR 400.7 + CR 302.6: Single authority for ETB state. Haste granted
        // below via `extra_keywords` (Twinflame, etc.) is folded in at query
        // time by `has_summoning_sickness`.
        token.reset_for_battlefield_entry(state.turn_number);

        // CR 707.2 + CR 702: "except it has [keyword]" — grant additional
        // keywords on top of the copied characteristics. Twinflame's haste
        // copies are the canonical case. Idempotent under repeats.
        for kw in &extra_keywords {
            if !token.keywords.contains(kw) {
                token.keywords.push(kw.clone());
            }
            if !token.base_keywords.contains(kw) {
                token.base_keywords.push(kw.clone());
            }
        }

        token.tapped = enter_tapped.resolve(tapped);
        let _ = token;
        let finalization = CopyTokenFinalization {
            name: name.clone(),
            enters_attacking,
            source_id,
            controller,
        };
        if !apply_token_modifications(
            state,
            token_id,
            &finalization,
            &additional_modifications,
            events,
        ) {
            let remaining_count = final_count.saturating_sub(index + 1);
            if remaining_count > 0 {
                super::counters::append_pending_counter_post_actions(
                    state,
                    vec![PendingCounterPostAction::ContinueCopyTokenCreation {
                        owner: token_owner,
                        copy: Box::new(CopyTokenSpec {
                            values: values.clone(),
                            display_source,
                            printed_ref: printed_ref.clone(),
                            token_image_ref: token_image_ref.clone(),
                            extra_keywords: extra_keywords.clone(),
                            additional_modifications: additional_modifications.clone(),
                            tapped,
                            enters_attacking,
                            sacrifice_at: sacrifice_at.clone(),
                            source_id,
                            controller,
                        }),
                        enter_tapped,
                        enter_with_counters: enter_with_counters.clone(),
                        remaining_count,
                    }],
                );
            }
            state.last_created_token_ids = created_ids.clone();
            return CopyTokenApplyStatus {
                created_ids,
                completion: CopyTokenApplyCompletion::Paused,
            };
        }

        finalize_copied_token(state, source_id, token_id);

        // CR 614.1c + CR 122.6a: ETB-counter replacement mutations are carried
        // on the accepted CreateToken spec, even for copy tokens whose full
        // CR 707 payload lives in `CopyTokenSpec`.
        for (counter_index, (counter_type, counter_count)) in etb_counters.iter().enumerate() {
            if *counter_count > 0
                && !super::counters::add_counter_with_replacement(
                    state,
                    token_owner,
                    token_id,
                    counter_type.clone(),
                    *counter_count,
                    events,
                )
            {
                state.last_created_token_ids = created_ids.clone();
                let remaining_counters = etb_counters[counter_index + 1..]
                    .iter()
                    .filter(|(_, count)| *count > 0)
                    .map(|(counter_type, count)| {
                        crate::types::game_state::PendingCounterAddition::Object {
                            actor: token_owner,
                            object_id: token_id,
                            counter_type: counter_type.clone(),
                            count: *count,
                        }
                    })
                    .collect();
                let remaining_count = final_count.saturating_sub(index + 1);
                super::counters::stash_pending_counter_additions(
                    state,
                    remaining_counters,
                    crate::types::game_state::PendingEffectResolved::with_post_actions_without_effect(
                        EffectKind::CopyTokenOf,
                        source_id,
                        vec![
                            PendingCounterPostAction::FinalizeCopyTokenEntry {
                                object_id: token_id,
                                name: name.clone(),
                                enters_attacking,
                                source_id,
                                controller,
                            },
                            PendingCounterPostAction::ContinueCopyTokenCreation {
                                owner: token_owner,
                                copy: Box::new(CopyTokenSpec {
                                    values: values.clone(),
                                    display_source,
                                    printed_ref: printed_ref.clone(),
                                    token_image_ref: token_image_ref.clone(),
                                    extra_keywords: extra_keywords.clone(),
                                    additional_modifications: additional_modifications.clone(),
                                    tapped,
                                    enters_attacking,
                                    sacrifice_at: sacrifice_at.clone(),
                                    source_id,
                                    controller,
                                }),
                                enter_tapped,
                                enter_with_counters: enter_with_counters.clone(),
                                remaining_count,
                            },
                        ],
                    ),
                );
                return CopyTokenApplyStatus {
                    created_ids,
                    completion: CopyTokenApplyCompletion::Paused,
                };
            }
        }

        // CR 508.4: Uses shared helper for defending player resolution.
        if enters_attacking {
            crate::game::combat::enter_attacking(state, token_id, source_id, controller);
        }

        // CR 111.10: Predefined token abilities for known subtypes (Treasure, Food, etc.).
        super::token::inject_predefined_token_abilities(state, token_id);
        // Battlefield entry of a copy token: request an incremental re-derive
        // for just this token. `flush_layers` escalates to a full pass when
        // the copied object sources a continuous effect, carries a CDA, etc.
        crate::game::layers::mark_layers_entered(state, token_id);
        crate::game::restrictions::record_battlefield_entry(state, token_id);
        crate::game::restrictions::record_token_created(state, token_id);

        let zone_change_record = state
            .objects
            .get(&token_id)
            .expect("token just created")
            .snapshot_for_zone_change(token_id, None, Zone::Battlefield);
        events.push(GameEvent::ZoneChanged {
            object_id: token_id,
            from: None,
            to: Zone::Battlefield,
            record: Box::new(zone_change_record),
        });
        events.push(GameEvent::TokenCreated {
            object_id: token_id,
            name: name.clone(),
            source_id,
        });
        created_ids.push(token_id);
    }

    CopyTokenApplyStatus {
        created_ids,
        completion: CopyTokenApplyCompletion::Completed,
    }
}

pub(crate) struct CopyTokenApplyStatus {
    pub(crate) created_ids: Vec<ObjectId>,
    pub(crate) completion: CopyTokenApplyCompletion,
}

pub(crate) enum CopyTokenApplyCompletion {
    Completed,
    Paused,
}

struct CopyTokenFinalization {
    name: String,
    enters_attacking: bool,
    source_id: ObjectId,
    controller: crate::types::player::PlayerId,
}

/// CR 707.2 / CR 707.9: Complete copy-token entry and apply remaining copy
/// modifications after resuming from a counter-placement replacement pause.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_remaining_token_modifications_after_counter_pause(
    state: &mut GameState,
    token_id: ObjectId,
    name: String,
    enters_attacking: bool,
    source_id: ObjectId,
    controller: crate::types::player::PlayerId,
    remaining_modifications: Vec<ContinuousModification>,
    events: &mut Vec<GameEvent>,
) -> bool {
    let finalization = CopyTokenFinalization {
        name: name.clone(),
        enters_attacking,
        source_id,
        controller,
    };
    if !apply_token_modifications(
        state,
        token_id,
        &finalization,
        &remaining_modifications,
        events,
    ) {
        return false;
    }
    finalize_copied_token(state, source_id, token_id);
    if enters_attacking {
        crate::game::combat::enter_attacking(state, token_id, source_id, controller);
    }
    super::token::inject_predefined_token_abilities(state, token_id);
    crate::game::layers::mark_layers_entered(state, token_id);
    crate::game::restrictions::record_battlefield_entry(state, token_id);
    crate::game::restrictions::record_token_created(state, token_id);
    if let Some(token) = state.objects.get(&token_id) {
        let zone_change_record = token.snapshot_for_zone_change(token_id, None, Zone::Battlefield);
        events.push(GameEvent::ZoneChanged {
            object_id: token_id,
            from: None,
            to: Zone::Battlefield,
            record: Box::new(zone_change_record),
        });
    }
    events.push(GameEvent::TokenCreated {
        object_id: token_id,
        name,
        source_id,
    });
    state.last_created_token_ids.push(token_id);
    if let Some(pending) = state.pending_copy_token_resolution.as_mut() {
        pending.created_ids.push(token_id);
    }
    true
}

/// CR 707.2: Compute the longest contiguous prefix of `source_ids` (top-down
/// resolution order) whose copy sources all share IDENTICAL copiable values.
///
/// Tier-3 batch support: a run of "create a token that's a copy of it"
/// self-copy triggers from distinct sources produces N tokens with identical
/// characteristics iff every source has the same CR 707.2 copiable values. This
/// walks the run, snapshots the top source's copiable values, then extends the
/// prefix while each subsequent source's values are `==` to the snapshot.
///
/// Conserves on a vanished source: if `compute_current_copiable_values` returns
/// `None` for any source in the prefix walk, the prefix stops there (the top
/// source returning `None` yields `None` overall — nothing to batch).
///
/// Returns `(prefix_values, prefix_len)`. `prefix_len` may be shorter than
/// `source_ids.len()` (a divergent tail resolves later). Token art is read from
/// the live source at resolution time (`token_copy::resolve`), so no display
/// `PrintedCardRef` is threaded through the batch probe (CR 707.2: not a
/// copiable characteristic).
pub(crate) fn compute_copy_batch_prefix(
    state: &GameState,
    source_ids: &[ObjectId],
) -> Option<(crate::types::ability::CopiableValues, u32)> {
    let top_id = *source_ids.first()?;
    if !has_active_copy_layer_effects(state) {
        let top = state.objects.get(&top_id)?;
        let prefix_values = intrinsic_copiable_values(top);
        let mut prefix_len = 1u32;
        for &id in source_ids.iter().skip(1) {
            let Some(obj) = state.objects.get(&id) else {
                break;
            };
            if intrinsic_copiable_values(obj) == prefix_values {
                prefix_len += 1;
            } else {
                break;
            }
        }
        return Some((prefix_values, prefix_len));
    }

    // Conserve on a vanished top source.
    let prefix_values = compute_current_copiable_values(state, top_id)?;

    let mut prefix_len = 1u32;
    for &id in source_ids.iter().skip(1) {
        // CR 707.2: stop at the first source that vanished (None) or whose
        // copiable values diverge from the prefix snapshot.
        match compute_current_copiable_values(state, id) {
            Some(values) if values == prefix_values => prefix_len += 1,
            _ => break,
        }
    }

    Some((prefix_values, prefix_len))
}

/// CR 707.2 + CR 707.9: Apply non-keyword `, except <body>` modifications to
/// a synthesized token. Tokens are created with copiable values baked in, so
/// each modification mutates BOTH the layered view (`card_types`,
/// `keywords`, etc.) AND the base view (`base_card_types`, `base_keywords`)
/// directly — there is no "before exception" state to layer over the way a
/// `BecomeCopy` modification layers over an existing object.
///
/// Variants consumed here:
/// - `RemoveSupertype` / `AddSupertype` — Miirym, Sentinel Wyrm; Sarkhan-class.
/// - `AddCounterOnEnter` — Spark Double-class. Counter placed via the shared
///   `counters::add_counter_with_replacement` primitive (which handles
///   replacements such as Doubling Season).
/// - `SetName` — copy-name override (rare for token-copy, harmless if present).
/// - `AddType` / `RemoveType` / `AddSubtype` / `RemoveSubtype` — type
///   exception support for token-copy (compose with type-modifying except
///   bodies that share grammar with `BecomeCopy`).
/// - `SetCardTypes` — Myrkul, Lord of Bones: "it's an enchantment and loses
///   all other card types" replaces the copied core card-type set (CR 613.1d).
/// - `AddKeyword` is NOT consumed here — keywords flow through the typed
///   `extra_keywords` channel earlier in the resolver.
///
/// Modifications not relevant to token-copy semantics (e.g. `CopyValues`,
/// `ChangeController`, dynamic P/T) are skipped silently — they have no
/// meaningful "stamp at creation" interpretation. A future card with such
/// an except body will surface as an unimplemented modification, which is
/// strictly better than silently mutating the token incorrectly.
fn apply_token_modifications(
    state: &mut GameState,
    token_id: ObjectId,
    finalization: &CopyTokenFinalization,
    modifications: &[ContinuousModification],
    events: &mut Vec<GameEvent>,
) -> bool {
    for (index, modification) in modifications.iter().enumerate() {
        match modification {
            // CR 205.4 + CR 707.9b: "the token isn't legendary" (Miirym class).
            ContinuousModification::RemoveSupertype { supertype } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.card_types.supertypes.retain(|s| s != supertype);
                    token.base_card_types.supertypes.retain(|s| s != supertype);
                }
            }
            // CR 205.4 + CR 707.9d: "it's <supertype> in addition to its other types".
            ContinuousModification::AddSupertype { supertype } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    if !token.card_types.supertypes.contains(supertype) {
                        token.card_types.supertypes.push(*supertype);
                    }
                    if !token.base_card_types.supertypes.contains(supertype) {
                        token.base_card_types.supertypes.push(*supertype);
                    }
                }
            }
            // CR 122.1 + CR 614.1c: Counter at creation, optionally gated by
            // the resolved core type. Read core types from the just-stamped
            // `card_types` (already includes any AddType/RemoveType applied
            // earlier in this loop) before placing the counter.
            ContinuousModification::AddCounterOnEnter {
                counter_type,
                count,
                if_type,
            } => {
                let counter_actor = state
                    .objects
                    .get(&token_id)
                    .map(|o| o.controller)
                    .unwrap_or(crate::types::player::PlayerId(0));
                let n = resolve_quantity(state, count, counter_actor, token_id).max(0) as u32;
                if n == 0 {
                    continue;
                }
                let gate_passes = match if_type {
                    None => true,
                    Some(t) => state
                        .objects
                        .get(&token_id)
                        .map(|obj| obj.card_types.core_types.contains(t))
                        .unwrap_or(false),
                };
                if !gate_passes {
                    continue;
                }
                if !super::counters::add_counter_with_replacement(
                    state,
                    counter_actor,
                    token_id,
                    counter_type.clone(),
                    n,
                    events,
                ) {
                    super::counters::stash_pending_counter_post_actions(
                        state,
                        EffectKind::CopyTokenOf,
                        finalization.source_id,
                        vec![
                            PendingCounterPostAction::ApplyCopyTokenModificationsAndFinalize {
                                object_id: token_id,
                                name: finalization.name.clone(),
                                enters_attacking: finalization.enters_attacking,
                                source_id: finalization.source_id,
                                controller: finalization.controller,
                                remaining_modifications: modifications[index + 1..].to_vec(),
                            },
                        ],
                    );
                    return false;
                }
            }
            // CR 707.9b: Name override applied at copy time.
            ContinuousModification::SetName { name } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.name = name.clone();
                    token.base_name = name.clone();
                }
            }
            // CR 205.1a: Type/subtype additions/removals as copy exceptions.
            ContinuousModification::AddType { core_type } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    if !token.card_types.core_types.contains(core_type) {
                        token.card_types.core_types.push(*core_type);
                    }
                    if !token.base_card_types.core_types.contains(core_type) {
                        token.base_card_types.core_types.push(*core_type);
                    }
                }
            }
            ContinuousModification::RemoveType { core_type } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.card_types.core_types.retain(|t| t != core_type);
                    token.base_card_types.core_types.retain(|t| t != core_type);
                }
            }
            ContinuousModification::AddSubtype { subtype } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    if !token.card_types.subtypes.iter().any(|s| s == subtype) {
                        token.card_types.subtypes.push(subtype.clone());
                    }
                    if !token.base_card_types.subtypes.iter().any(|s| s == subtype) {
                        token.base_card_types.subtypes.push(subtype.clone());
                    }
                }
            }
            ContinuousModification::RemoveSubtype { subtype } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.card_types.subtypes.retain(|s| s != subtype);
                    token.base_card_types.subtypes.retain(|s| s != subtype);
                }
            }
            // CR 707.9b + CR 613.1d: a copy exception with no "in addition"
            // carve-out replaces the copied creature subtypes (CR 707.9d). The
            // wiped set becomes part of the token's copiable values, so apply it
            // to both live and base subtype lists.
            ContinuousModification::RemoveAllSubtypes { set } => {
                let all_creature_types = &state.all_creature_types;
                let objects = &mut state.objects;
                if let Some(token) = objects.get_mut(&token_id) {
                    remove_subtype_set(&mut token.card_types.subtypes, *set, all_creature_types);
                    remove_subtype_set(
                        &mut token.base_card_types.subtypes,
                        *set,
                        all_creature_types,
                    );
                }
            }
            // CR 205.1a + CR 613.1d + CR 707.9d: "it's an enchantment and loses
            // all other card types" (Myrkul, Lord of Bones) REPLACES the copied
            // card's core card-type set. Supertypes (Legendary) are retained;
            // subtypes are filtered through the shared `subtype_matches_core_types`
            // rule so this baked path keeps exactly the subtypes the layered
            // `SetCardTypes` arm would (uncorrelated noncreature subtypes drop).
            // Stamped into both live and base card types so the override is part
            // of the token's copiable values (CR 707.9b).
            ContinuousModification::SetCardTypes { core_types } => {
                let all_creature_types = &state.all_creature_types;
                let objects = &mut state.objects;
                if let Some(token) = objects.get_mut(&token_id) {
                    token.card_types.core_types = core_types.clone();
                    token.base_card_types.core_types = core_types.clone();
                    let keep = |subtype: &String| {
                        crate::game::layers::subtype_matches_core_types(
                            subtype,
                            core_types,
                            all_creature_types,
                        )
                    };
                    token.card_types.subtypes.retain(|s| keep(s));
                    token.base_card_types.subtypes.retain(|s| keep(s));
                }
            }
            // CR 707.9b + CR 613.1e: a copy exception that sets color (no
            // "in addition to its other colors" carve-out, CR 707.9d) replaces
            // the copied color. The result becomes part of the token's copiable
            // values, so set both live and base color.
            ContinuousModification::SetColor { colors } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.color = colors.clone();
                    token.base_color = colors.clone();
                }
            }
            // CR 707.9 + CR 202.1b: "except it has no mana cost" — strip the
            // copied mana cost so the token's mana value is 0 (Embalm
            // CR 702.128a, Eternalize CR 702.129a). Set both live and base so
            // the override is part of the token's copiable values.
            ContinuousModification::RemoveManaCost => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.mana_cost = crate::types::mana::ManaCost::NoCost;
                    token.base_mana_cost = crate::types::mana::ManaCost::NoCost;
                }
            }
            // CR 707.9b + CR 613.1e: a copy exception that adds color
            // ("in addition to its other colors") becomes part of the token's
            // copiable values without removing the copied color.
            ContinuousModification::AddColor { color } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    if !token.color.contains(color) {
                        token.color.push(*color);
                    }
                    if !token.base_color.contains(color) {
                        token.base_color.push(*color);
                    }
                }
            }
            // CR 707.9b: "except it's 1/1" — set base and live P/T so the
            // override persists through layer resets. Used by Offspring
            // (CR 702.175a) and Saw in Half.
            ContinuousModification::SetPower { value } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_power = Some(*value);
                    token.power = Some(*value);
                }
            }
            ContinuousModification::SetToughness { value } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_toughness = Some(*value);
                    token.toughness = Some(*value);
                }
            }
            // CR 707.9b: fixed additive P/T exceptions are baked into the
            // token's copiable values by updating both base and live P/T.
            ContinuousModification::AddPower { value } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_power = token.base_power.map(|p| p + *value);
                    token.power = token.power.map(|p| p + *value);
                }
            }
            ContinuousModification::AddToughness { value } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_toughness = token.base_toughness.map(|t| t + *value);
                    token.toughness = token.toughness.map(|t| t + *value);
                }
            }
            // CR 707.9b: "except its base power and toughness are each equal
            // to half [X]" (Saw in Half). Dynamic quantity resolved at
            // creation time and stamped as base P/T.
            ContinuousModification::SetPowerDynamic { value } => {
                let controller = state
                    .objects
                    .get(&token_id)
                    .map(|o| o.controller)
                    .unwrap_or(crate::types::player::PlayerId(0));
                let val = resolve_quantity(state, value, controller, token_id);
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_power = Some(val);
                    token.power = Some(val);
                }
            }
            ContinuousModification::SetToughnessDynamic { value } => {
                let controller = state
                    .objects
                    .get(&token_id)
                    .map(|o| o.controller)
                    .unwrap_or(crate::types::player::PlayerId(0));
                let val = resolve_quantity(state, value, controller, token_id);
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_toughness = Some(val);
                    token.toughness = Some(val);
                }
            }
            // CR 707.9b + CR 306.5b/c: Starting-loyalty exceptions are already
            // consumed before loyalty counters are seeded, but apply the live
            // fields idempotently so a paused/resumed modification sequence
            // keeps the stamped token coherent.
            ContinuousModification::SetStartingLoyalty { value } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_loyalty = Some(*value);
                    token.loyalty = Some(*value);
                }
            }
            // CR 707.9a + CR 603.1: "except it has \"<triggered ability>\""
            // (Chandra, Flameshaper [+1]: "…and \"At the beginning of the end
            // step, sacrifice this token.\""). The granted trigger becomes part
            // of the copy's copiable values, so stamp it onto both the live and
            // base trigger sets. `SelfRef`/`This`-anchored triggers in the
            // grant resolve against the token because they fire with the token
            // as their source.
            ContinuousModification::GrantTrigger { trigger } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.trigger_definitions.push((**trigger).clone());
                    Arc::make_mut(&mut token.base_trigger_definitions).push((**trigger).clone());
                }
            }
            // CR 707.9a: "except it has \"<activated/static ability>\"" — the
            // granted ability is part of the copy's copiable values. Mirrors the
            // GrantTrigger arm for the `obj.abilities` list.
            ContinuousModification::GrantAbility { definition } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    Arc::make_mut(&mut token.abilities).push((**definition).clone());
                    Arc::make_mut(&mut token.base_abilities).push((**definition).clone());
                }
            }
            // CR 707.9a + CR 604.1: quoted static text in a copy exception is
            // also part of the token's copiable values. Mirror predefined token
            // rules text by carrying static-rule modifications on a self static.
            ContinuousModification::GrantStaticAbility { .. }
            | ContinuousModification::AddStaticMode { .. } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    let static_def = StaticDefinition::continuous()
                        .affected(TargetFilter::SelfRef)
                        .modifications(vec![modification.clone()]);
                    Arc::make_mut(&mut token.base_static_definitions).push(static_def.clone());
                    token.static_definitions.push(static_def);
                }
            }
            // CR 707.9a + CR 702: a keyword granted via an except body that
            // landed in `additional_modifications` (rather than `extra_keywords`)
            // — e.g. a keyword adjacent to a quoted-ability grant. Apply it to
            // the copiable keyword set, idempotently, mirroring `extra_keywords`.
            ContinuousModification::AddKeyword { keyword } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    if !token.keywords.contains(keyword) {
                        token.keywords.push(keyword.clone());
                    }
                    if !token.base_keywords.contains(keyword) {
                        token.base_keywords.push(keyword.clone());
                    }
                }
            }
            // Other layered-only modifications (CopyValues, ChangeController,
            // etc.) are intentionally skipped — their "stamp at copy time"
            // interpretation is ambiguous, and a future except body needing
            // them should route through the BecomeCopy layered path instead.
            _ => {}
        }
    }
    true
}

pub(crate) fn copy_starting_loyalty_override(
    modifications: &[ContinuousModification],
) -> Option<u32> {
    modifications.iter().rev().find_map(|modification| {
        if let ContinuousModification::SetStartingLoyalty { value } = modification {
            Some(*value)
        } else {
            None
        }
    })
}

/// CR 205.1a + CR 613.1d: remove every subtype belonging to the given
/// [`SubtypeSet`] from a token's subtype list. Creature types are recognised
/// against the game's live `all_creature_types` list (Changeling / set-defined
/// types are runtime data); every other set has a fixed CR-defined membership.
fn remove_subtype_set(subtypes: &mut Vec<String>, set: SubtypeSet, all_creature_types: &[String]) {
    match set {
        // CR 205.3m: creature types.
        SubtypeSet::Creature => {
            subtypes.retain(|s| {
                !all_creature_types
                    .iter()
                    .any(|creature_type| creature_type == s)
            });
        }
        SubtypeSet::Land => subtypes.retain(|s| !crate::types::card_type::is_land_subtype(s)),
        SubtypeSet::Artifact => {
            subtypes.retain(|s| !crate::types::card_type::ARTIFACT_SUBTYPES.contains(&s.as_str()))
        }
        SubtypeSet::Enchantment => subtypes
            .retain(|s| !crate::types::card_type::ENCHANTMENT_SUBTYPES.contains(&s.as_str())),
        SubtypeSet::Planeswalker => subtypes
            .retain(|s| !crate::types::card_type::PLANESWALKER_SUBTYPES.contains(&s.as_str())),
        SubtypeSet::Spell => {
            subtypes.retain(|s| !crate::types::card_type::SPELL_SUBTYPES.contains(&s.as_str()));
        }
        SubtypeSet::Battle => {
            subtypes.retain(|s| !crate::types::card_type::BATTLE_SUBTYPES.contains(&s.as_str()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::game_object::DisplaySource;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, AdditionalCostPaymentSource, ContinuousModification,
        ControllerRef, CostPaidObjectSnapshot, Effect, FilterProp, ObjectScope, PtValue,
        QuantityExpr, QuantityModification, QuantityRef, ReplacementDefinition, RoundingMode,
        TargetFilter, TargetRef, TypeFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card::PrintedCardRef;
    use crate::types::card_type::{CardType, CoreType, Supertype};
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::{ObjectId, TrackedSetId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::triggers::TriggerMode;

    /// CR 707.9b + CR 707.9d: a copy token whose exception sets P/T, replaces
    /// color, and replaces creature subtypes (The Scarab God shape) stamps each
    /// characteristic onto both the live and base (copiable) values of the
    /// synthesized token.
    #[test]
    fn copy_token_exceptions_stamp_pt_color_and_subtype() {
        let mut state = GameState::new_two_player(42);
        state.all_creature_types = vec![
            "Human".to_string(),
            "Soldier".to_string(),
            "Zombie".to_string(),
        ];
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Elite Vanguard".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(2);
            source.base_toughness = Some(1);
            source.power = Some(2);
            source.toughness = Some(1);
            source.base_color = vec![ManaColor::White];
            source.color = vec![ManaColor::White];
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Human".to_string(), "Soldier".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![
                    ContinuousModification::SetPower { value: 4 },
                    ContinuousModification::SetToughness { value: 4 },
                    ContinuousModification::SetColor {
                        colors: vec![ManaColor::Black],
                    },
                    ContinuousModification::RemoveAllSubtypes {
                        set: SubtypeSet::Creature,
                    },
                    ContinuousModification::AddType {
                        core_type: CoreType::Creature,
                    },
                    ContinuousModification::AddSubtype {
                        subtype: "Zombie".to_string(),
                    },
                ],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert_eq!(token.power, Some(4));
        assert_eq!(token.toughness, Some(4));
        assert_eq!(token.color, vec![ManaColor::Black]);
        assert!(token.card_types.subtypes.contains(&"Zombie".to_string()));
        assert!(!token.card_types.subtypes.contains(&"Human".to_string()));
        assert!(!token.card_types.subtypes.contains(&"Soldier".to_string()));
        assert_eq!(token.base_power, Some(4));
        assert_eq!(token.base_toughness, Some(4));
        assert_eq!(token.base_color, vec![ManaColor::Black]);
        assert!(token
            .base_card_types
            .subtypes
            .contains(&"Zombie".to_string()));
        assert!(!token
            .base_card_types
            .subtypes
            .contains(&"Human".to_string()));
        assert!(!token
            .base_card_types
            .subtypes
            .contains(&"Soldier".to_string()));
    }

    /// CR 205.1a + CR 613.1d + CR 707.9d: Myrkul, Lord of Bones — "create a
    /// token that's a copy of that card, except it's an enchantment and loses
    /// all other card types." `SetCardTypes` replaces the copied creature's
    /// core types with `[Enchantment]` (no longer a creature), while supertypes
    /// (Legendary) survive. Subtype retention follows the shared
    /// `subtype_matches_core_types` rule used by the layered path: a noncreature
    /// subtype not correlated to the new core types (here the artifact subtype
    /// "Equipment") drops, keeping both applications consistent. Applied to both
    /// live and base (copiable) card types.
    #[test]
    fn copy_token_set_card_types_replaces_core_types_and_drops_uncorrelated_subtype() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dying God".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(7);
            source.base_toughness = Some(5);
            source.power = Some(7);
            source.toughness = Some(5);
            source.base_card_types = CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Artifact, CoreType::Creature],
                subtypes: vec!["Equipment".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::SetCardTypes {
                    core_types: vec![CoreType::Enchantment],
                }],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        // Core types replaced: enchantment only, no longer a creature or artifact.
        assert_eq!(token.card_types.core_types, vec![CoreType::Enchantment]);
        assert_eq!(
            token.base_card_types.core_types,
            vec![CoreType::Enchantment]
        );
        // CR 205.1a: the "Equipment" artifact subtype is no longer correlated, so it drops.
        assert!(!token.card_types.subtypes.contains(&"Equipment".to_string()));
        assert!(!token
            .base_card_types
            .subtypes
            .contains(&"Equipment".to_string()));
        // Supertypes are unaffected by a card-type replacement.
        assert!(token.card_types.supertypes.contains(&Supertype::Legendary));
        assert!(token
            .base_card_types
            .supertypes
            .contains(&Supertype::Legendary));
    }

    #[test]
    fn copy_token_of_self_creates_copy() {
        let mut state = GameState::new_two_player(42);

        // Create a creature to copy
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mist-Syndicate Naga".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(3);
            source.base_toughness = Some(1);
            source.power = Some(3);
            source.toughness = Some(1);
            source.base_color = vec![ManaColor::Blue];
            source.color = vec![ManaColor::Blue];
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Snake".to_string(), "Ninja".to_string()],
            };
            source.card_types = source.base_card_types.clone();
            source.base_keywords = vec![Keyword::Ninjutsu(Default::default())];
            source.keywords = source.base_keywords.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // Find the token (it's the newest object)
        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();

        assert_eq!(token.name, "Mist-Syndicate Naga");
        assert_eq!(token.power, Some(3));
        assert_eq!(token.toughness, Some(1));
        assert_eq!(token.color, vec![ManaColor::Blue]);
        assert!(token.card_types.core_types.contains(&CoreType::Creature));
        assert!(token.card_types.subtypes.contains(&"Snake".to_string()));
        assert!(token.is_token);
        assert!(token.zone == Zone::Battlefield);
        assert!(state.layers_dirty.is_dirty());
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::TokenCreated { name, .. } if name == "Mist-Syndicate Naga")
        ));
        // Verify record_battlefield_entry and record_token_created were called
        assert!(
            state
                .players_who_created_token_this_turn
                .contains(&PlayerId(0)),
            "should record token creation"
        );
    }

    /// CR 614.1a + CR 707.2: A token-count-doubling replacement (Doubling
    /// Season / Adrix and Nev / Parallel Lives / Anointed Procession / Mondrak)
    /// applies to a token that's a *copy* of a permanent, exactly as it applies
    /// to a predefined `Effect::Token`. Such doublers are CR 614.1a replacement
    /// effects that modify the number of tokens created; copy-token creation
    /// (CR 707.5 / CR 707.2) is a token-creation event, so the same replacement
    /// applies: the doubling is applied first, then each copy enters with its
    /// own ETB. Issue #1511 regression: `CopyTokenOf` previously created exactly
    /// `count` copies, bypassing the `ProposedEvent::CreateToken` replacement
    /// pipeline, so the doubler never saw the copy.
    #[test]
    fn copy_token_count_doubling_replacement_applies() {
        let mut state = GameState::new_two_player(42);

        // Doubling-Season-style mandatory token-count doubler, controller-scoped.
        let doubler_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        {
            let doubler = state.objects.get_mut(&doubler_id).unwrap();
            let def = ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .token_owner_scope(ControllerRef::You)
                .quantity_modification(QuantityModification::Double);
            doubler.base_replacement_definitions = Arc::new(vec![def.clone()]);
            doubler.replacement_definitions = vec![def].into();
        }

        // The copy source — a 3/1 Snake.
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mist-Syndicate Naga".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(3);
            source.base_toughness = Some(1);
            source.power = Some(3);
            source.toughness = Some(1);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Snake".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 614.1a: count 1 doubled to 2 — two independent copy tokens.
        let copies: Vec<_> = state
            .objects
            .values()
            .filter(|o| o.is_token && o.name == "Mist-Syndicate Naga")
            .collect();
        assert_eq!(
            copies.len(),
            2,
            "token-count doubler must double a copy-token's count (issue #1511)"
        );
        // Each doubled copy enters with its own faithful characteristics + ETB.
        assert!(copies
            .iter()
            .all(|t| t.power == Some(3) && t.toughness == Some(1)));
        assert!(copies.iter().all(|t| t.zone == Zone::Battlefield));
        assert_eq!(
            state.last_created_token_ids.len(),
            2,
            "both doubled copy-token ids are recorded for downstream anaphora"
        );
        // CR 603.6a: each copy emits its own TokenCreated/ETB event.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e,
                    GameEvent::TokenCreated { name, .. } if name == "Mist-Syndicate Naga"
                ))
                .count(),
            2,
            "each doubled copy emits its own ETB/TokenCreated event"
        );
    }

    /// CR 616.1 + CR 707.2: If copy-token creation is modified by
    /// order-material token-count replacements, the resolver must pause for the
    /// affected player's choice and then resume by creating real copy tokens,
    /// not generic probe tokens.
    #[test]
    fn copy_token_replacement_choice_resumes_with_copy_payload() {
        let mut state = GameState::new_two_player(42);

        let doubler_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        {
            let doubler = state.objects.get_mut(&doubler_id).unwrap();
            let def = ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .token_owner_scope(ControllerRef::You)
                .quantity_modification(QuantityModification::Double);
            doubler.base_replacement_definitions = Arc::new(vec![def.clone()]);
            doubler.replacement_definitions = vec![def].into();
        }

        let plus_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Token Augmenter".to_string(),
            Zone::Battlefield,
        );
        {
            let plus = state.objects.get_mut(&plus_id).unwrap();
            let def = ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .token_owner_scope(ControllerRef::You)
                .quantity_modification(QuantityModification::Plus { value: 1 });
            plus.base_replacement_definitions = Arc::new(vec![def.clone()]);
            plus.replacement_definitions = vec![def].into();
        }

        let source_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Glasspool Mimic".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.printed_ref = Some(PrintedCardRef {
                oracle_id: "glasspool-oracle".to_string(),
                face_name: "Glasspool Mimic".to_string(),
            });
            source.base_printed_ref = source.printed_ref.clone();
            source.display_source = DisplaySource::Card;
            source.base_power = Some(3);
            source.base_toughness = Some(3);
            source.power = Some(3);
            source.toughness = Some(3);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Shapeshifter".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::ReplacementChoice {
                    candidate_count: 2,
                    ..
                }
            ),
            "non-commuting copy-token count replacements must prompt for CR 616 order"
        );
        assert!(
            state.last_created_token_ids.is_empty(),
            "no copy token should be created before the replacement choice resolves"
        );

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).unwrap();

        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "copy-token resolution should finish after the replacement choice"
        );
        assert!(
            (3..=4).contains(&state.last_created_token_ids.len()),
            "chosen Double/Plus ordering should create three or four copies, not the unmodified one"
        );
        for token_id in &state.last_created_token_ids {
            let token = state.objects.get(token_id).unwrap();
            assert!(token.is_token);
            assert_eq!(token.name, "Glasspool Mimic");
            assert_eq!(token.power, Some(3));
            assert_eq!(token.toughness, Some(3));
            assert_eq!(token.display_source, DisplaySource::Card);
            assert_eq!(
                token
                    .printed_ref
                    .as_ref()
                    .map(|printed| printed.face_name.as_str()),
                Some("Glasspool Mimic"),
                "replacement-choice resume must use the copy payload, not generic TokenSpec apply"
            );
        }
    }

    /// CR 614.1c + CR 707.2: ETB-counter replacement mutations live on the
    /// accepted `CreateToken` event's `TokenSpec`; copy-token apply must consume
    /// them in addition to the CR 707 copy payload.
    #[test]
    fn copy_token_creation_applies_etb_counter_replacement_payload() {
        let mut state = GameState::new_two_player(42);

        let counter_replacement_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Mentor".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&counter_replacement_id).unwrap();
            let def = ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                .valid_card(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                ))
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PutCounter {
                        target: TargetFilter::SelfRef,
                        counter_type: CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                ));
            source.base_replacement_definitions = Arc::new(vec![def.clone()]);
            source.replacement_definitions = vec![def].into();
        }

        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Runeclaw Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(2);
            source.base_toughness = Some(2);
            source.power = Some(2);
            source.toughness = Some(2);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = state.last_created_token_ids[0];
        let token = state.objects.get(&token_id).unwrap();
        assert_eq!(token.name, "Runeclaw Bear");
        assert_eq!(
            token.counters.get(&CounterType::Plus1Plus1).copied(),
            Some(1),
            "copy-token apply must consume accepted TokenSpec enter_with_counters"
        );
    }

    /// Non-regression: without any token-count replacement active,
    /// `CopyTokenOf { count: N }` creates exactly N copies.
    #[test]
    fn copy_token_count_without_doubler_is_exact() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(2);
            source.base_toughness = Some(2);
            source.power = Some(2);
            source.toughness = Some(2);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 3 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let copies = state
            .objects
            .values()
            .filter(|o| o.is_token && o.name == "Bear")
            .count();
        assert_eq!(copies, 3, "no doubler: exactly the requested count");
    }

    #[test]
    fn copy_token_propagates_printed_ref_for_image_lookup() {
        // A copy of a real-card permanent must carry the source's Scryfall
        // image hint (oracle_id + displayed face_name) so the frontend resolves
        // the same art. Regression: copying an MDFC face (The Prismatic Bridge)
        // produced a token with `printed_ref: None`, which rendered blank in the
        // legend-rule chooser because the back-face name is absent from the
        // front-face-only image index.
        let mut state = GameState::new_two_player(42);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "The Prismatic Bridge".to_string(),
            Zone::Battlefield,
        );
        let source_ref = crate::types::card::PrintedCardRef {
            oracle_id: "92023a5d-a143-4950-a71b-d736e6b8e959".to_string(),
            face_name: "The Prismatic Bridge".to_string(),
        };
        state.objects.get_mut(&source_id).unwrap().printed_ref = Some(source_ref.clone());

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        assert!(state.objects[&token_id].is_token);
        assert_eq!(
            state.objects[&token_id].printed_ref,
            Some(source_ref.clone()),
            "token copy must carry the source's printed_ref for image lookup"
        );

        // The fix is only durable if the token also carries `base_printed_ref`:
        // the layer reset restores `printed_ref` from the baseline each pass, so
        // without it the next `evaluate_layers` would wipe the art back to None.
        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&token_id].printed_ref,
            Some(source_ref),
            "token copy's printed_ref must survive a layer evaluation pass"
        );
    }

    #[test]
    fn copy_token_of_target_creates_copy() {
        let mut state = GameState::new_two_player(42);

        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.base_power = Some(2);
            target.base_toughness = Some(2);
            target.power = Some(2);
            target.toughness = Some(2);
        }

        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Copier".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(target_id)],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert_eq!(token.name, "Grizzly Bears");
        assert_eq!(token.power, Some(2));
        assert_eq!(token.toughness, Some(2));
        assert!(token.is_token);
    }

    /// Issue #2402: Hazel of the Rootbloom — copy a non-Squirrel token target
    /// whose live characteristics must be mirrored into `base_*` fields by the
    /// real token creation path before copy-token resolution reads copiable values.
    #[test]
    fn issue_2402_copy_token_of_token_target_creates_copy() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Hazel".to_string(),
            Zone::Battlefield,
        );
        let create_food = ResolvedAbility::new(
            Effect::Token {
                name: "Food".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".to_string(), "Food".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::token::resolve(&mut state, &create_food, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);
        let food = state.last_created_token_ids[0];
        let food_token = state.objects.get(&food).unwrap();
        assert!(food_token.base_characteristics_initialized);
        assert_eq!(food_token.base_name, "Food");
        assert_eq!(
            food_token.base_card_types.core_types,
            vec![CoreType::Artifact]
        );
        assert_eq!(food_token.base_card_types.subtypes, vec!["Food"]);

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(food)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let copy_id = ObjectId(state.next_object_id - 1);
        let copy = state.objects.get(&copy_id).unwrap();
        assert!(copy.is_token);
        assert_eq!(copy.name, "Food");
        assert_eq!(copy.card_types.core_types, vec![CoreType::Artifact]);
        assert_eq!(copy.card_types.subtypes, vec!["Food"]);
    }

    /// CR 109.4 + CR 111.2: "target opponent creates a token that's a copy of
    /// it" — the copy token must enter under the chosen opponent's control,
    /// not the trigger controller's. Pins the new `owner` channel at the
    /// building-block level (issue #403 defect 1).
    #[test]
    fn copy_token_of_owner_creates_under_chosen_player() {
        let mut state = GameState::new_two_player(42);

        // The copy source — a permanent controlled by PlayerId(0).
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Wedding Ring".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                // Copy source stays Wedding Ring itself.
                target: TargetFilter::SelfRef,
                // Non-context-ref owner filter — resolved from `ability.targets`.
                owner: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default()
                        .controller(crate::types::ability::ControllerRef::Opponent),
                ),
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            // The chosen opponent target.
            vec![TargetRef::Player(PlayerId(1))],
            source_id,
            // Wedding Ring's trigger controller is PlayerId(0).
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        // The token is a copy of Wedding Ring (the source), not a player.
        assert_eq!(token.name, "Wedding Ring");
        assert!(token.is_token);
        // CR 109.4: the token is controlled (and owned) by the chosen opponent.
        assert_eq!(
            token.controller,
            PlayerId(1),
            "copy token must be controlled by the chosen opponent, not the trigger controller"
        );
        assert_eq!(token.owner, PlayerId(1));
    }

    /// CR 109.4: the default `owner` of `TargetFilter::Controller` keeps the
    /// copy under the resolving ability's controller (the common case —
    /// populate, "you create a token that's a copy of …").
    #[test]
    fn copy_token_of_default_owner_is_controller() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert_eq!(token.controller, PlayerId(0));
    }

    /// CR 609.3 + CR 101.3: An unattached Springheart Nantuko resolves
    /// `CopyTokenOf { target: AttachedTo }` with no host — `AttachedTo`
    /// resolves empty. The effect must be a clean zero-token no-op (no token
    /// created, `Ok` not `Err`) so the chained Insect-token fallback can fire.
    #[test]
    fn copy_token_of_empty_host_is_clean_no_op() {
        let mut state = GameState::new_two_player(42);
        // Source object with no `attached_to` — `AttachedTo` resolves empty.
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Springheart Nantuko".to_string(),
            Zone::Battlefield,
        );
        let objects_before = state.objects.len();

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::AttachedTo,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("empty host must be a clean no-op");

        assert_eq!(
            state.objects.len(),
            objects_before,
            "no token may be created when the AttachedTo host is empty"
        );
        assert!(
            state.last_created_token_ids.is_empty(),
            "no token ids recorded for an empty-host no-op"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::EffectResolved { .. })),
            "EffectResolved must still be emitted so the chain proceeds"
        );
    }

    #[test]
    fn copy_token_of_cost_paid_object_creates_requested_copies() {
        let mut state = GameState::new_two_player(42);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Osgir, the Reconstructor".to_string(),
            Zone::Battlefield,
        );
        let artifact_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Ichor Wellspring".to_string(),
            Zone::Exile,
        );
        {
            let artifact = state.objects.get_mut(&artifact_id).unwrap();
            artifact.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec![],
            };
            artifact.card_types = artifact.base_card_types.clone();
        }

        let snapshot = {
            let artifact = state.objects.get(&artifact_id).unwrap();
            CostPaidObjectSnapshot {
                object_id: artifact_id,
                lki: artifact.snapshot_for_mana_spent(),
            }
        };
        let mut ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::CostPaidObject,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 2 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        ability.set_cost_paid_object_recursive(snapshot);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let copies: Vec<_> = state
            .objects
            .values()
            .filter(|object| object.is_token && object.name == "Ichor Wellspring")
            .collect();
        assert_eq!(copies.len(), 2);
        assert!(copies.iter().all(|token| token.zone == Zone::Battlefield));
        assert!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::TokenCreated { name, .. } if name == "Ichor Wellspring"
                ))
                .count()
                >= 2
        );
    }

    /// CR 603.10a / Vaultborn Tyrant + Ochre Jelly class: LTB self-copy triggers
    /// fire after the source has moved to the graveyard. The parsed effect is
    /// `CopyTokenOf { target: ParentTarget }` with empty `ability.targets`; the
    /// resolver must copy the source object from the graveyard.
    #[test]
    fn copy_token_of_parent_target_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Vaultborn Tyrant".to_string(),
            Zone::Graveyard,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(6);
            source.base_toughness = Some(6);
            source.power = Some(6);
            source.toughness = Some(6);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dinosaur".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::ParentTarget,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        assert_eq!(token.name, "Vaultborn Tyrant");
        assert_eq!(token.power, Some(6));
        assert_eq!(token.toughness, Some(6));
        // Source remains in graveyard (we only copy it, we don't move it).
        assert_eq!(state.objects[&source_id].zone, Zone::Graveyard);
    }

    /// CR 603.7 + CR 707.2: "copy of that card" after an exile instruction
    /// must read the tracked set published by the prior zone change. Copy
    /// sources are zone-agnostic, so an exiled card is a valid source.
    #[test]
    fn copy_token_of_tracked_set_source_from_exile() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Kheru Goldkeeper".to_string(),
            Zone::Exile,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(3);
            source.base_toughness = Some(3);
            source.power = Some(3);
            source.toughness = Some(3);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Zombie".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![source_id]);

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(1),
                },
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: true,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        assert!(token.tapped);
        assert_eq!(token.name, "Kheru Goldkeeper");
        assert_eq!(token.power, Some(3));
        assert_eq!(token.toughness, Some(3));
    }

    #[test]
    fn copy_token_enters_tapped_and_attacking() {
        let mut state = GameState::new_two_player(42);

        // Set up combat
        state.combat = Some(crate::game::combat::CombatState::default());

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(2);
            source.base_toughness = Some(2);
            source.power = Some(2);
            source.toughness = Some(2);
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: true,
                tapped: true,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();

        // CR 508.4: Token enters tapped and attacking
        assert!(token.tapped);
        let combat = state.combat.as_ref().unwrap();
        assert!(combat.attackers.iter().any(|a| a.object_id == token_id));
    }

    /// CR 707.2 + CR 702.10 (Haste): Twinflame's "except it has haste" — copy
    /// tokens carry the source's keywords plus the granted extra keyword.
    #[test]
    fn copy_token_extra_keywords_grant_haste() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_power = Some(2);
            s.base_toughness = Some(2);
            s.power = Some(2);
            s.toughness = Some(2);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![Keyword::Haste],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        assert!(token.keywords.contains(&Keyword::Haste));
        assert!(token.base_keywords.contains(&Keyword::Haste));
    }

    /// CR 115.1d + CR 601.2c: Twinflame's "for each of them" — multi-target
    /// CopyTokenOf creates one copy per object in `ability.targets`, and all
    /// created token IDs are recorded in `state.last_created_token_ids` so the
    /// "those tokens" anaphor in the delayed exile trigger captures the full
    /// set.
    #[test]
    fn copy_token_multi_target_creates_one_per_target() {
        let mut state = GameState::new_two_player(42);
        let bear_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear A".to_string(),
            Zone::Battlefield,
        );
        let bear_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear B".to_string(),
            Zone::Battlefield,
        );
        for id in [bear_a, bear_b] {
            let s = state.objects.get_mut(&id).unwrap();
            s.base_power = Some(2);
            s.base_toughness = Some(2);
            s.power = Some(2);
            s.toughness = Some(2);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }
        let twinflame_src = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Twinflame".to_string(),
            Zone::Stack,
        );
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::ParentTarget,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![Keyword::Haste],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(bear_a), TargetRef::Object(bear_b)],
            twinflame_src,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        // Two new tokens, both with haste.
        assert_eq!(state.last_created_token_ids.len(), 2);
        for token_id in &state.last_created_token_ids {
            let t = state.objects.get(token_id).unwrap();
            assert!(t.is_token);
            assert!(t.keywords.contains(&Keyword::Haste));
        }
        // Names follow each respective source.
        let names: Vec<&str> = state
            .last_created_token_ids
            .iter()
            .map(|id| state.objects[id].name.as_str())
            .collect();
        assert!(names.contains(&"Bear A"));
        assert!(names.contains(&"Bear B"));
    }

    #[test]
    fn copy_token_source_filter_copies_matching_tokens_not_source() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 5;
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Ocelot Pride".to_string(),
            Zone::Battlefield,
        );
        let cat_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Cat".to_string(),
            Zone::Battlefield,
        );
        let old_cat_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Old Cat".to_string(),
            Zone::Battlefield,
        );
        let opponent_cat_id = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Opponent Cat".to_string(),
            Zone::Battlefield,
        );
        for (id, turn) in [
            (cat_id, Some(5)),
            (old_cat_id, Some(4)),
            (opponent_cat_id, Some(5)),
        ] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.is_token = true;
            obj.entered_battlefield_turn = turn;
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Cat".to_string()],
            };
            obj.card_types = obj.base_card_types.clone();
        }
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::ParentTarget,
                owner: TargetFilter::Controller,
                source_filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![],
                    controller: Some(ControllerRef::You),
                    properties: vec![FilterProp::Token, FilterProp::EnteredThisTurn],
                })),
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.last_created_token_ids.len(), 1);
        let copied = state.objects.get(&state.last_created_token_ids[0]).unwrap();
        assert_eq!(copied.name, "Cat");
        assert!(copied.is_token);
    }

    /// CR 205.4 + CR 707.9b + CR 704.5j: Miirym, Sentinel Wyrm class —
    /// `additional_modifications: [RemoveSupertype(Legendary)]` strips the
    /// Legendary supertype from the synthesized token. The legend rule
    /// (CR 704.5j) only collapses legendary permanents, so two such tokens
    /// must coexist on the battlefield without state-based action collapse.
    #[test]
    fn copy_token_remove_supertype_strips_legendary_from_token() {
        let mut state = GameState::new_two_player(42);
        // Source is a legendary creature (e.g., a Dragon).
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bahamut".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_power = Some(7);
            s.base_toughness = Some(7);
            s.power = Some(7);
            s.toughness = Some(7);
            s.base_card_types = CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dragon".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }

        // Synthesize Miirym's CopyTokenOf with the RemoveSupertype modification.
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::RemoveSupertype {
                    supertype: Supertype::Legendary,
                }],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        // Layered view: Legendary stripped.
        assert!(
            !token.card_types.supertypes.contains(&Supertype::Legendary),
            "token must not be Legendary; got {:?}",
            token.card_types.supertypes
        );
        // Base view: Legendary stripped from the copiable values too — the
        // exception is part of the copy effect's bake-in (CR 707.2), so future
        // copies-of-this-token also start without Legendary.
        assert!(
            !token
                .base_card_types
                .supertypes
                .contains(&Supertype::Legendary),
            "token's base_card_types must not contain Legendary; got {:?}",
            token.base_card_types.supertypes
        );
    }

    /// CR 704.5j + CR 707.9b: Issue #685 regression. When token-copy strips
    /// the Legendary supertype via `additional_modifications`, the legend
    /// rule SBA must NOT prompt the controller to choose which copy to
    /// sacrifice — the token is no longer Legendary, so there is exactly one
    /// Legendary permanent with the shared name (the original). Both
    /// permanents must remain on the battlefield. This is the SBA-side
    /// counterpart to the parser-side fix for the contracted "it's not
    /// legendary" form (Delina, Wild Mage; Ratadrabik of Urborg; etc.).
    #[test]
    fn legend_rule_does_not_fire_when_copy_token_drops_legendary() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bahamut".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_power = Some(7);
            s.base_toughness = Some(7);
            s.power = Some(7);
            s.toughness = Some(7);
            s.base_card_types = CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dragon".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::RemoveSupertype {
                    supertype: Supertype::Legendary,
                }],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = state.last_created_token_ids[0];

        // Run state-based actions; the legend rule SBA must NOT fire because
        // the token is not Legendary.
        let mut sba_events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut sba_events);

        assert!(
            !matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::ChooseLegend { .. }
            ),
            "legend rule must not present a choice when token is not legendary; \
             got waiting_for={:?}",
            state.waiting_for
        );
        assert_eq!(
            state.objects[&source_id].zone,
            Zone::Battlefield,
            "original legendary creature must remain on battlefield"
        );
        assert_eq!(
            state.objects[&token_id].zone,
            Zone::Battlefield,
            "non-legendary token-copy must remain on battlefield"
        );
    }

    /// CR 122.1 + CR 614.1c: AddCounterOnEnter with matching `if_type` places
    /// the counter on the synthesized token. Spark Double's planeswalker copy
    /// branch is exercised at the BecomeCopy resolver site; this test pins
    /// the same primitive on the token-copy path.
    #[test]
    fn copy_token_add_counter_on_enter_unconditional() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Soldier".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_power = Some(2);
            s.base_toughness = Some(2);
            s.power = Some(2);
            s.toughness = Some(2);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
            s.card_types = s.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::AddCounterOnEnter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    if_type: None,
                }],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        let p1p1 = token
            .counters
            .get(&crate::types::counter::CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            p1p1, 1,
            "token should have one +1/+1 counter; counters={:?}",
            token.counters
        );
    }

    #[test]
    fn paused_copy_token_add_counter_on_enter_preserves_remaining_batch() {
        let mut state = GameState::new_two_player(42);
        let replacement_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Choice".to_string(),
            Zone::Battlefield,
        );
        {
            let mut def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .valid_card(TargetFilter::Any)
                .quantity_modification(QuantityModification::Prevent);
            def.mode = crate::types::ability::ReplacementMode::Optional { decline: None };
            let obj = state.objects.get_mut(&replacement_source).unwrap();
            obj.base_replacement_definitions = Arc::new(vec![def.clone()]);
            obj.replacement_definitions = vec![def].into();
        }

        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Soldier".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(2);
            source.base_toughness = Some(2);
            source.power = Some(2);
            source.toughness = Some(2);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Soldier".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 2 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::AddCounterOnEnter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    if_type: None,
                }],
            },
            vec![TargetRef::Object(source_id)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));

        let mut choice_events = Vec::new();
        for _ in 0..2 {
            let result =
                apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).unwrap();
            choice_events.extend(result.events);
        }

        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(
            state.last_created_token_ids.len(),
            2,
            "copy-token counter pauses must preserve the current token and remaining copies"
        );
        for token_id in &state.last_created_token_ids {
            let token = state.objects.get(token_id).unwrap();
            assert!(token.is_token);
            assert_eq!(token.name, "Soldier");
        }
        assert_eq!(
            choice_events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::CopyTokenOf,
                        source_id: ObjectId(100),
                    }
                ))
                .count(),
            1,
            "the copy-token effect should resolve once after the paused batch finishes"
        );
    }

    /// CR 707.9f: Conditional `if_type` declines when the resolved object's
    /// core type doesn't match. Token-copy of a non-creature with
    /// `AddCounterOnEnter { if_type: Some(Creature) }` must NOT place the
    /// counter (mirrors Spark Double's "if it's a creature" branch on a
    /// planeswalker copy).
    #[test]
    fn copy_token_add_counter_on_enter_if_type_mismatch_skips() {
        let mut state = GameState::new_two_player(42);
        // Copy source: a planeswalker (no Creature core type).
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jace".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_loyalty = Some(3);
            s.loyalty = Some(3);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Planeswalker],
                subtypes: vec!["Jace".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::AddCounterOnEnter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    if_type: Some(CoreType::Creature),
                }],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        let p1p1 = token
            .counters
            .get(&crate::types::counter::CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            p1p1, 0,
            "if_type=Creature must skip on a Planeswalker copy; counters={:?}",
            token.counters
        );
    }

    /// CR 306.5b + CR 306.5c + CR 707.2: A token that's a copy of a planeswalker
    /// must enter with loyalty counters equal to the copied loyalty — the copy
    /// path builds the object directly, bypassing the ZoneChange ETB-counter
    /// seeding, so it must seed loyalty counters itself. Pre-fix the copy carried
    /// only the `loyalty` field with no counter, so once the layer system treats
    /// the counter map as the source of truth (CR 306.5c) the copy would read 0
    /// loyalty and die immediately to CR 704.5i.
    #[test]
    fn copy_token_of_planeswalker_seeds_loyalty_counters() {
        use crate::game::layers::evaluate_layers;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jace".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_loyalty = Some(5);
            s.loyalty = Some(5);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Planeswalker],
                subtypes: vec!["Jace".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        assert_eq!(
            state.objects[&token_id]
                .counters
                .get(&CounterType::Loyalty)
                .copied(),
            Some(5),
            "planeswalker copy must seed loyalty counters equal to copied loyalty",
        );

        // The copy must survive a layer re-evaluation at its real loyalty, not
        // snap to 0 (it's still on the battlefield, not in the graveyard).
        evaluate_layers(&mut state);
        let token = &state.objects[&token_id];
        assert_eq!(token.zone, Zone::Battlefield);
        assert_eq!(
            token.loyalty,
            Some(5),
            "copied planeswalker loyalty must derive from its seeded counters",
        );
    }

    /// CR 707.9b + CR 306.5b/c: Jace, Mirror Mage's token-copy exception sets
    /// the copy's starting loyalty to 1. This is not an extra loyalty counter;
    /// it replaces the loyalty value used for intrinsic planeswalker counter
    /// seeding, so the token must enter with one loyalty counter even if the
    /// copied source has a different loyalty value.
    #[test]
    fn copy_token_starting_loyalty_exception_overrides_seeded_loyalty() {
        use crate::game::layers::evaluate_layers;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jace, Mirror Mage".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_loyalty = Some(5);
            source.loyalty = Some(5);
            source.base_card_types = CardType {
                supertypes: vec![crate::types::card_type::Supertype::Legendary],
                core_types: vec![CoreType::Planeswalker],
                subtypes: vec!["Jace".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![
                    ContinuousModification::RemoveSupertype {
                        supertype: crate::types::card_type::Supertype::Legendary,
                    },
                    ContinuousModification::SetStartingLoyalty { value: 1 },
                ],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = &state.objects[&token_id];
        assert_eq!(token.base_loyalty, Some(1));
        assert_eq!(
            token.counters.get(&CounterType::Loyalty).copied(),
            Some(1),
            "starting-loyalty exception must seed one loyalty counter"
        );
        assert!(!token
            .card_types
            .supertypes
            .contains(&crate::types::card_type::Supertype::Legendary));

        evaluate_layers(&mut state);
        let token = &state.objects[&token_id];
        assert_eq!(token.zone, Zone::Battlefield);
        assert_eq!(token.loyalty, Some(1));
    }

    /// Regression: Helm of the Host (DOM, MH3, BLC) — pin the already-shipped
    /// non-legendary token-copy behavior so a future refactor cannot silently
    /// drop the `RemoveSupertype { Legendary }` stamp.
    ///
    /// Helm of the Host's begin-combat trigger creates a token that's a copy
    /// of equipped creature, "except the token isn't legendary." When the
    /// equipped creature IS legendary, the synthesized token must not be
    /// legendary — both the layered view (`card_types.supertypes`) and the
    /// copiable-values view (`base_card_types.supertypes`) must be free of
    /// `Supertype::Legendary`. Otherwise the legend rule (CR 704.5j) would
    /// collapse the token alongside its source.
    ///
    /// This test exercises the resolver with Helm's full ability shape:
    /// `Effect::CopyTokenOf { target: Typed[Creature]+EquippedBy,
    /// additional_modifications: [RemoveSupertype(Legendary)] }`. The general
    /// resolver behavior is also pinned by
    /// `copy_token_remove_supertype_strips_legendary_from_token` (Miirym
    /// class); this test anchors the named card so the behavior cannot
    /// regress without an explicit failure pointing at Helm of the Host.
    ///
    /// CR 707.9b + CR 205.4 + CR 301.5a: copy modifications, supertype
    /// semantics, and the equipped-creature relationship.
    #[test]
    fn helm_of_the_host_token_copy_strips_legendary_from_equipped_creature() {
        let mut state = GameState::new_two_player(42);

        // Equipped creature: a legendary 7/7 Dragon (e.g., Bahamut).
        let equipped_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bahamut".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&equipped_id).unwrap();
            s.base_power = Some(7);
            s.base_toughness = Some(7);
            s.power = Some(7);
            s.toughness = Some(7);
            s.base_card_types = CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dragon".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }

        // Helm of the Host: non-legendary Equipment artifact attached to the
        // equipped creature. The trigger source for the begin-combat trigger.
        let helm_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Helm of the Host".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&helm_id).unwrap();
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Equipment".to_string()],
            };
            s.card_types = s.base_card_types.clone();
            s.attached_to = Some(equipped_id.into());
        }

        // Resolve Helm's begin-combat trigger: CopyTokenOf with the exact
        // Helm AST shape (`target: Typed[Creature]+EquippedBy`,
        // `additional_modifications: [RemoveSupertype(Legendary)]`). After
        // trigger resolution the engine has bound `EquippedBy` to the
        // equipped creature, so the resolved ability carries
        // `targets: [Object(equipped_id)]`.
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![FilterProp::EquippedBy],
                }),
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::RemoveSupertype {
                    supertype: Supertype::Legendary,
                }],
            },
            vec![TargetRef::Object(equipped_id)],
            helm_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();

        // CR 707.2: token copies the equipped creature's name, P/T, and types.
        assert!(token.is_token);
        assert_eq!(token.name, "Bahamut");
        assert_eq!(token.power, Some(7));
        assert_eq!(token.toughness, Some(7));

        // CR 707.9b + CR 205.4: layered view has Legendary stripped.
        assert!(
            !token.card_types.supertypes.contains(&Supertype::Legendary),
            "token must not be Legendary; got supertypes={:?}",
            token.card_types.supertypes
        );

        // CR 707.9b: copiable-values view also has Legendary stripped — the
        // exception is part of the copy effect's bake-in, so future copies
        // of this token also start without Legendary.
        assert!(
            !token
                .base_card_types
                .supertypes
                .contains(&Supertype::Legendary),
            "token's base_card_types must not contain Legendary; got {:?}",
            token.base_card_types.supertypes
        );

        // CR 704.5j: with the original legendary creature and the
        // non-legendary token-copy both on the battlefield, the legend rule
        // SBA must NOT fire — there is exactly one Legendary permanent named
        // "Bahamut" (the source); the token shares the name but is not
        // legendary, so it is not a candidate for collapse.
        let mut sba_events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut sba_events);

        assert!(
            !matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::ChooseLegend { .. }
            ),
            "legend rule must not present a choice when the token is not legendary; \
             got waiting_for={:?}",
            state.waiting_for
        );
        // Both permanents survive on the battlefield.
        assert_eq!(
            state.objects[&equipped_id].zone,
            Zone::Battlefield,
            "original legendary creature must remain on battlefield"
        );
        assert_eq!(
            state.objects[&token_id].zone,
            Zone::Battlefield,
            "non-legendary token-copy must remain on battlefield"
        );
    }

    /// CR 702.175a: Offspring creates a token that's a copy of the creature,
    /// except it's 1/1. `SetPower`/`SetToughness` in `additional_modifications`
    /// must override the copied base P/T at creation time.
    #[test]
    fn offspring_token_is_1_1_not_copy_pt() {
        let mut state = GameState::new_two_player(42);

        // Create a 3/2 creature (the "parent" with offspring).
        let parent_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Coruscation Mage".to_string(),
            Zone::Battlefield,
        );
        {
            let parent = state.objects.get_mut(&parent_id).unwrap();
            parent.base_power = Some(3);
            parent.base_toughness = Some(2);
            parent.power = Some(3);
            parent.toughness = Some(2);
            parent.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Human".to_string(), "Wizard".to_string()],
            };
            parent.card_types = parent.base_card_types.clone();
        }

        let mut events = Vec::new();

        // Simulate the offspring ETB trigger: CopyTokenOf with SetPower(1), SetToughness(1).
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![
                    ContinuousModification::SetPower { value: 1 },
                    ContinuousModification::SetToughness { value: 1 },
                ],
            },
            vec![],
            parent_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // Find the token (newest object).
        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();

        // Token must be 1/1, not 3/2.
        assert_eq!(
            token.base_power,
            Some(1),
            "offspring token base_power must be 1"
        );
        assert_eq!(
            token.base_toughness,
            Some(1),
            "offspring token base_toughness must be 1"
        );
        assert_eq!(token.power, Some(1), "offspring token power must be 1");
        assert_eq!(
            token.toughness,
            Some(1),
            "offspring token toughness must be 1"
        );
        // Name and types are still copied.
        assert_eq!(token.name, "Coruscation Mage");
        assert!(token.card_types.subtypes.contains(&"Wizard".to_string()));
        assert!(token.is_token);
        assert!(
            !token
                .keywords
                .iter()
                .any(|kw| matches!(kw, crate::types::keywords::Keyword::Offspring(_))),
            "offspring token must not retain the cast-only Offspring keyword"
        );
        assert!(
            !token.trigger_definitions.iter_all().any(|trig| matches!(
                trig.condition,
                Some(TriggerCondition::AdditionalCostPaid { .. })
            )),
            "offspring token must not retain AdditionalCostPaid ETB triggers"
        );
    }

    /// CR 707.2 vs CR 601.2f + CR 603.4: copy-token finalization strips only
    /// cast-payment-gated triggers ("if its offspring cost was paid" / "if it
    /// was kicked"). Persistent spell-cast observers — Magecraft's
    /// `SpellCastOrCopy`, generic "whenever you cast a [type] spell"
    /// `SpellCast` — are copiable battlefield text and must survive on the
    /// token copy.
    #[test]
    fn copy_token_keeps_spell_cast_triggers_strips_payment_gated_triggers() {
        let mut state = GameState::new_two_player(42);

        let parent_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Archmage Emeritus".to_string(),
            Zone::Battlefield,
        );
        {
            let parent = state.objects.get_mut(&parent_id).unwrap();
            parent.base_power = Some(2);
            parent.base_toughness = Some(2);
            parent.power = Some(2);
            parent.toughness = Some(2);
            parent.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Human".to_string(), "Wizard".to_string()],
            };
            parent.card_types = parent.base_card_types.clone();

            // Magecraft-style persistent battlefield trigger (CR 707.10 note:
            // observes casts/copies; it is not itself cast-time bookkeeping).
            let magecraft = TriggerDefinition::new(TriggerMode::SpellCastOrCopy).description(
                "Magecraft — Whenever you cast or copy an instant or sorcery spell, draw a card."
                    .to_string(),
            );
            // Offspring-style ETB trigger gated on a cast-time payment.
            let offspring_etb = TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(TargetFilter::SelfRef)
                .condition(TriggerCondition::AdditionalCostPaid {
                    source: AdditionalCostPaymentSource::Any,
                    origin: None,
                    origin_ordinal: None,
                    variant: None,
                    kicker_cost: None,
                    min_count: 1,
                })
                .description("offspring etb".to_string());
            parent.base_trigger_definitions = Arc::new(vec![magecraft, offspring_etb]);
            parent.trigger_definitions = Arc::clone(&parent.base_trigger_definitions).into();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            parent_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        assert!(
            token
                .trigger_definitions
                .iter_all()
                .any(|trig| matches!(trig.mode, TriggerMode::SpellCastOrCopy)),
            "token copy must keep the persistent Magecraft SpellCastOrCopy trigger (CR 707.2)"
        );
        assert!(
            token
                .base_trigger_definitions
                .iter()
                .any(|trig| matches!(trig.mode, TriggerMode::SpellCastOrCopy)),
            "copiable base triggers must also keep the SpellCastOrCopy trigger (CR 707.2)"
        );
        assert!(
            !token.trigger_definitions.iter_all().any(|trig| matches!(
                trig.condition,
                Some(TriggerCondition::AdditionalCostPaid { .. })
            )),
            "token copy must strip cast-payment-gated triggers (CR 601.2f + CR 603.4)"
        );
    }

    /// CR 707.9b: dynamic copy exceptions are resolved after the copied values
    /// are stamped onto the token, then baked into the token's base P/T.
    #[test]
    fn copy_token_dynamic_pt_exception_uses_copied_values() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sawed Beast".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(5);
            source.base_toughness = Some(4);
            source.power = Some(5);
            source.toughness = Some(4);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Beast".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![
                    ContinuousModification::SetPowerDynamic {
                        value: QuantityExpr::DivideRounded {
                            inner: Box::new(QuantityExpr::Ref {
                                qty: QuantityRef::Power {
                                    scope: ObjectScope::Source,
                                },
                            }),
                            divisor: 2,
                            rounding: RoundingMode::Up,
                        },
                    },
                    ContinuousModification::SetToughnessDynamic {
                        value: QuantityExpr::DivideRounded {
                            inner: Box::new(QuantityExpr::Ref {
                                qty: QuantityRef::Toughness {
                                    scope: ObjectScope::Source,
                                },
                            }),
                            divisor: 2,
                            rounding: RoundingMode::Up,
                        },
                    },
                ],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert_eq!(token.base_power, Some(3));
        assert_eq!(token.power, Some(3));
        assert_eq!(token.base_toughness, Some(2));
        assert_eq!(token.toughness, Some(2));
        assert_eq!(token.name, "Sawed Beast");
        assert!(token.is_token);
    }
}
