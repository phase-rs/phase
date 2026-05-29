use crate::types::ability::{
    CastingPermission, ContinuousModification, Duration, EffectKind, KeywordAction,
    ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CastingVariant, ExileLink, ExileLinkKind, GameState, StackEntry, StackEntryKind,
    StackPaidSnapshot,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::ability_utils::{flatten_targets_in_chain, validate_targets_in_chain};
use super::effects;
use super::targeting;
use super::zones;

/// CR 405.1: Add an object to the stack.
pub fn push_to_stack(state: &mut GameState, entry: StackEntry, events: &mut Vec<GameEvent>) {
    events.push(GameEvent::StackPushed {
        object_id: entry.id,
    });
    state.stack.push_back(entry);
}

fn restore_alternative_spell_normal_face(state: &mut GameState, object_id: ObjectId) {
    if let Some(obj) = state.objects.get_mut(&object_id) {
        if let Some(normal_face) = obj.back_face.take() {
            let alternative_snapshot = super::printed_cards::snapshot_object_face(obj);
            super::printed_cards::apply_back_face_to_object(obj, normal_face);
            obj.back_face = Some(alternative_snapshot);
        }
    }
}

/// CR 608.2n / CR 608.3 / CR 608.3e: Predicate guard for post-resolution
/// default-zone moves on a resolving spell.
///
/// Spells normally leave the stack as the final part of their resolution —
/// non-permanents go to the graveyard (CR 608.2n), permanents enter the
/// battlefield (CR 608.3), and permanents whose ETB was fully prevented go
/// to the graveyard (CR 608.3e). Each of these default destinations is
/// itself a `move_to_zone(state, id, default, events)` call that runs
/// AFTER `execute_effect` has already had a chance to move the spell
/// elsewhere via its own instructions (e.g., Treasured Find — "Exile ~",
/// or any sub-ability that targets the source via `SelfRef`).
///
/// If the spell's resolution already moved it off the Stack, the default
/// move must be skipped — otherwise the card travels (Exile→Graveyard,
/// Exile→Battlefield, etc.) and undoes its own self-move clause (issue
/// #323). The Stack-residency check is the canonical guard: only spells
/// still on the Stack at the end of resolution receive the post-resolution
/// default destination.
fn spell_still_on_stack(state: &GameState, id: ObjectId) -> bool {
    spell_in_zone(state, id, Zone::Stack)
}

fn spell_in_zone(state: &GameState, id: ObjectId, zone: Zone) -> bool {
    state.objects.get(&id).is_some_and(|obj| obj.zone == zone)
}

fn move_prevented_permanent_spell_to_graveyard_if_still_on_stack(
    state: &mut GameState,
    id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    if spell_still_on_stack(state, id) {
        zones::move_to_zone(state, id, Zone::Graveyard, events);
    }
}

/// CR 608.2: Resolve the top object on the stack.
pub fn resolve_top(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // CR 603.3c + CR 603.3d: The top of the stack may be a trigger entry that
    // is still being constructed (mode / target / division pending). Such an
    // entry MUST NOT resolve — it is mid-flight while the controller is
    // gathering inputs via the active `WaitingFor`. The
    // `pending_trigger_entry` cursor is cleared when construction completes
    // (target chosen, distribution assigned, etc.); only then is resolution
    // permitted.
    if let Some(pending_id) = state.pending_trigger_entry {
        if state.stack.back().map(|e| e.id) == Some(pending_id) {
            return;
        }
    }

    // CR 707.10: A fresh resolution invalidates any previously stashed
    // resolving entry. `resolving_stack_entry` is set below and must persist
    // across an optional-choice round-trip (the Chain cycle's "you may copy
    // this spell" defers the copy past a player decision, by which point the
    // spell has left the stack) — so it is cleared here at the start of the
    // *next* resolution rather than at the end of this one.
    state.resolving_stack_entry = None;

    // CR 405.5: When all players pass in succession, the top object on the stack resolves.
    let entry = match state.stack.pop_back() {
        Some(e) => e,
        None => return,
    };
    state.stack_paid_facts.remove(&entry.id);

    // CR 113.3b: Activated keyword abilities (Equip / Crew / Saddle / Station)
    // resolve via their typed payload — they have no ResolvedAbility/targets
    // to validate and no zone-change routing (the source stays where it is).
    // Returning early keeps the keyword-action branch out of the targeting /
    // fizzle / permanent-spell pipeline below.
    if let StackEntryKind::KeywordAction { action } = entry.kind {
        resolve_keyword_action(state, action, events);
        events.push(GameEvent::StackResolved {
            object_id: entry.id,
        });
        return;
    }

    let trigger_event_batch = state.stack_trigger_event_batches.remove(&entry.id);

    // CR 603.4: Intervening-if condition rechecked at resolution time.
    if let StackEntryKind::TriggeredAbility {
        condition: Some(ref condition),
        source_id,
        ref trigger_event,
        ..
    } = entry.kind
    {
        if !super::triggers::check_trigger_condition(
            state,
            condition,
            entry.controller,
            Some(source_id),
            trigger_event.as_ref(),
        ) {
            events.push(GameEvent::StackResolved {
                object_id: entry.id,
            });
            return;
        }
    }

    // CR 603.7c: Set trigger event context for event-context target resolution.
    // TriggeringSpellController, TriggeringSource, etc. read this during resolution.
    if let StackEntryKind::TriggeredAbility {
        trigger_event: Some(ref te),
        ..
    } = entry.kind
    {
        state.current_trigger_event = Some(te.clone());
        state.current_trigger_events = trigger_event_batch.unwrap_or_else(|| vec![te.clone()]);
    } else if let Some(trigger_events) = trigger_event_batch {
        state.current_trigger_event = trigger_events.first().cloned();
        state.current_trigger_events = trigger_events;
    }

    // CR 603.2c: Lift the filtered subject count of a batched trigger into
    // resolution scope so `QuantityRef::EventContextAmount` resolves "that
    // many" against the count, not against zero. Set in lockstep with
    // `current_trigger_event` and cleared at every reset site below.
    if let StackEntryKind::TriggeredAbility {
        subject_match_count,
        ..
    } = entry.kind
    {
        state.current_trigger_match_count = subject_match_count;
    }

    // Extract the resolved ability from the stack entry. `KeywordAction` is
    // handled by the early return above and never reaches this match.
    let (mut ability, is_spell, casting_variant, actual_mana_spent) = match &entry.kind {
        StackEntryKind::Spell {
            ability,
            casting_variant,
            actual_mana_spent,
            ..
        } => (ability.clone(), true, *casting_variant, *actual_mana_spent),
        StackEntryKind::ActivatedAbility { ability, .. } => {
            (Some(ability.clone()), false, CastingVariant::Normal, 0)
        }
        StackEntryKind::TriggeredAbility { ability, .. } => (
            Some(ResolvedAbility::clone(ability)),
            false,
            CastingVariant::Normal,
            0,
        ),
        StackEntryKind::KeywordAction { .. } => unreachable!(
            "KeywordAction stack entries are resolved via the early-return branch above"
        ),
    };

    // CR 603.7c + CR 120.3 + CR 506.2: A "deals [combat] damage to a player" /
    // "attacks a player" trigger introduces the damaged/attacked player as the
    // event referent. Stamp it onto the resolving ability's `scoped_player`
    // (when not already bound) so `PlayerScope::ScopedPlayer` quantities such as
    // "they lose half their life, rounded up" (Unstoppable Slasher) resolve
    // against that player rather than falling back to the source's controller.
    // Mirrors the Phase-trigger stamping in `triggers::build_triggered_ability`;
    // the parser rebinds these possessives to `ScopedPlayer` in
    // `lower_trigger_ir`.
    if let Some(ability) = ability.as_mut() {
        if ability.scoped_player.is_none() {
            if let Some(pid) = state.current_trigger_event.as_ref().and_then(|event| {
                matches!(
                    event,
                    GameEvent::DamageDealt {
                        target: TargetRef::Player(_),
                        ..
                    } | GameEvent::AttackersDeclared { .. }
                )
                .then(|| targeting::extract_player_from_event(event, state))
                .flatten()
            }) {
                ability.set_scoped_player_recursive(pid);
            }
        }
    }

    // Capture targets for Aura attachment after resolution
    let spell_targets = ability
        .as_ref()
        .map(|a| a.targets.clone())
        .unwrap_or_default();

    // CR 702.103e: As a bestowed Aura spell begins resolving, if its target is
    // illegal it ceases to be bestowed and the effect making it an Aura spell
    // ends — it continues resolving as a creature spell. We detect this BEFORE
    // the standard fizzle check (which would otherwise route the spell to
    // graveyard per CR 608.2b). The revert restores Creature core type and
    // removes the bestow-granted Aura subtype + `enchant creature` keyword;
    // `is_permanent_type` then sees a Creature and routes to the battlefield.
    let mut bestow_reverted_at_resolution = false;
    if casting_variant == CastingVariant::Bestow {
        let target_is_illegal = ability.as_ref().is_some_and(|a| {
            let original = flatten_targets_in_chain(a);
            if original.is_empty() {
                return false;
            }
            let validated = validate_targets_in_chain(state, a);
            let legal = flatten_targets_in_chain(&validated);
            targeting::check_fizzle(&original, &legal)
        });
        let still_bestow_form = state
            .objects
            .get(&entry.id)
            .is_some_and(|o| o.bestow_form.is_some());
        if target_is_illegal && still_bestow_form {
            super::casting::revert_bestow_form(state, entry.id);
            bestow_reverted_at_resolution = true;
        }
    }

    // CR 707.10: Expose the resolving stack entry so a `CopySpell` carried as
    // the spell's own effect (the Chain cycle's "you may copy this spell")
    // can copy itself even though `resolve_top` has already popped it off the
    // stack — and even after the spell has moved to the graveyard while an
    // optional copy decision is pending. Cleared at the start of the next
    // `resolve_top`.
    state.resolving_stack_entry = Some(entry.clone());

    // Only run targeting validation and effect execution when an ability exists.
    // Permanent spells with no spell ability (ability is None) skip straight to
    // zone-change handling below.
    if let Some(ref ability) = ability {
        let original_targets = flatten_targets_in_chain(ability);
        // CR 702.103e: when a bestowed Aura reverted at the start of resolution,
        // suppress the fizzle check — the spell is no longer an Aura and proceeds
        // to resolve as a creature spell with no remaining target.
        if !original_targets.is_empty() && !bestow_reverted_at_resolution {
            let validated = validate_targets_in_chain(state, ability);
            let legal_targets = flatten_targets_in_chain(&validated);
            if targeting::check_fizzle(&original_targets, &legal_targets) {
                // CR 608.2b: Fizzle — all targets illegal, spell is countered on resolution.
                if is_spell {
                    // CR 702.34a / CR 702.127a / CR 702.180a: Flashback,
                    // Aftermath, and Harmonize exile when leaving the stack
                    // for any reason, including fizzle. Escape (CR 702.138)
                    // has no such clause — escaped spells go to graveyard normally.
                    let dest = if casting_variant.replaces_stack_to_graveyard_with_exile() {
                        Zone::Exile
                    } else {
                        Zone::Graveyard
                    };
                    zones::move_to_zone(state, entry.id, dest, events);
                    if matches!(
                        casting_variant,
                        CastingVariant::Adventure | CastingVariant::Omen
                    ) {
                        restore_alternative_spell_normal_face(state, entry.id);
                    }
                }
                events.push(GameEvent::StackResolved {
                    object_id: entry.id,
                });
                state.current_trigger_event = None;
                state.current_trigger_events.clear();
                state.current_trigger_match_count = None;
                return;
            }
            execute_effect(state, &validated, events);
        } else {
            execute_effect(state, ability, events);
        }
    }

    // CR 702.xxx: Paradigm (Strixhaven) — first-resolution hook. If the
    // resolving spell carries `Keyword::Paradigm` and this is the first
    // resolution of any spell with this name by the controller (per the
    // reminder text: "After you first resolve a spell with this name"), arm
    // the Paradigm offer: push a `ParadigmPrime` record and mint an
    // `ExileLinkKind::ParadigmSource` link, then override destination routing
    // to Exile. Copies (`is_token`) never arm Paradigm because their card
    // name is derived but they are not "the" spell per the reminder. Assign
    // when WotC publishes SOS CR update.
    let paradigm_armed = if is_spell {
        let obj = state.objects.get(&entry.id);
        let has_paradigm = obj.is_some_and(|o| {
            !o.is_token
                && super::keywords::has_keyword(o, &crate::types::keywords::Keyword::Paradigm)
        });
        if has_paradigm {
            let card_name = obj.map(|o| o.name.clone()).unwrap_or_default();
            super::effects::paradigm::arm_paradigm(state, entry.id, entry.controller, &card_name)
        } else {
            false
        }
    } else {
        false
    };

    // CR 608.3: Determine destination zone for spells.
    if is_spell {
        let dest = if paradigm_armed {
            // CR 702.xxx: Paradigm-armed spell exiles instead of going to
            // graveyard. The ExileLink is already created by arm_paradigm.
            Zone::Exile
        } else if casting_variant == CastingVariant::Adventure {
            // CR 715.3d: Adventure spell resolves → exile with casting permission.
            Zone::Exile
        } else if casting_variant == CastingVariant::Omen {
            // CR 720.3d: Omen spell resolves → shuffle into owner's library.
            Zone::Library
        } else if casting_variant == CastingVariant::Harmonize {
            // CR 702.180a: If the harmonize cost was paid, exile this card instead of putting it anywhere else.
            if is_permanent_spell(state, entry.id) {
                Zone::Battlefield
            } else {
                Zone::Exile
            }
        } else if casting_variant == CastingVariant::Aftermath {
            // CR 702.127a: If an aftermath spell was cast from a graveyard,
            // exile it instead of putting it anywhere else any time it would
            // leave the stack.
            Zone::Exile
        } else if casting_variant == CastingVariant::Flashback {
            // CR 702.34a: If the flashback cost was paid, exile this card
            // instead of putting it anywhere else any time it would leave the stack.
            // Flashback only appears on instants/sorceries — unconditional exile is correct.
            Zone::Exile
        } else if casting_variant.replaces_stack_to_graveyard_with_exile()
            && !is_permanent_spell(state, entry.id)
        {
            // CR 614.1a + CR 608.2n: Graveyard-cast permission riders that
            // say "If a spell cast this way would be put into your graveyard,
            // exile it instead" replace the normal non-permanent resolution
            // destination. Permanent spells still resolve to the battlefield.
            Zone::Exile
        } else if is_permanent_spell(state, entry.id) {
            // CR 608.3: Permanent spells enter the battlefield.
            Zone::Battlefield
        } else if ability
            .as_ref()
            .is_some_and(|a| a.context.additional_cost_paid)
            && state.objects.get(&entry.id).is_some_and(|o| {
                o.keywords
                    .iter()
                    .any(|k| matches!(k, crate::types::keywords::Keyword::Buyback(_)))
            })
        {
            // CR 702.27a: If the buyback cost was paid, put this spell into its
            // owner's hand instead of into that player's graveyard as it resolves.
            // Buyback appears only on instants/sorceries, so this branch is
            // unreachable for permanent spells. Does NOT redirect on counter
            // (CR 701.5a) or fizzle (CR 608.2b) — buyback applies only "as it
            // resolves."
            Zone::Hand
        } else {
            // CR 608.2n: Non-permanent spells are put into owner's graveyard.
            Zone::Graveyard
        };
        if dest == Zone::Battlefield {
            // CR 614.1c + CR 608.3: Route battlefield entry through the replacement
            // pipeline so ETB replacements (saga lore counters, enter-tapped, etc.) fire.
            let mut proposed = crate::types::proposed_event::ProposedEvent::zone_change(
                entry.id,
                Zone::Stack,
                Zone::Battlefield,
                None,
            );
            // CR 702.190b: Sneak-cast permanent enters the battlefield tapped.
            // Seed the ZoneChange so ETB-tapped goes through the replacement
            // pipeline (CR 614.1c).
            if matches!(casting_variant, CastingVariant::Sneak { .. }) {
                if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                    enter_tapped,
                    ..
                } = &mut proposed
                {
                    *enter_tapped = crate::types::proposed_event::EtbTapState::Tapped;
                }
            }
            // CR 712.14a + CR 310.11b: If this spell was cast via an
            // ExileWithAltCost permission with `cast_transformed`, the
            // permanent enters the battlefield transformed (resolving to its
            // back face). Used by the Siege victory trigger.
            if let Some(obj) = state.objects.get(&entry.id) {
                let cast_transformed = obj.casting_permissions.iter().any(|p| {
                    matches!(
                        p,
                        CastingPermission::ExileWithAltCost {
                            cast_transformed: true,
                            ..
                        }
                    )
                });
                if cast_transformed {
                    if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                        enter_transformed,
                        ..
                    } = &mut proposed
                    {
                        *enter_transformed = true;
                    }
                }
                // CR 306.5b + CR 310.4b + CR 614.1c: Planeswalkers and battles
                // have the intrinsic replacement "This permanent enters with N
                // [loyalty/defense] counters on it." Seed these counters onto
                // the ZoneChange ProposedEvent so Doubling-Season-class
                // AddCounter replacements (CR 614.1a) see and modify them as
                // the replacement pipeline runs.
                let intrinsic = super::printed_cards::intrinsic_etb_counters(obj);
                if !intrinsic.is_empty() {
                    if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                        enter_with_counters,
                        ..
                    } = &mut proposed
                    {
                        enter_with_counters.extend(intrinsic);
                    }
                }
            }

            // CR 702.188a: Web-slinging is a casting alternative cost. Tag the
            // permanent BEFORE the ETB replacement pipeline runs so a
            // `ReplacementCondition::CastVariantPaid` gate (Scarlet Spider's
            // "Sensational Save" enters-with-counters replacement) can read it.
            // `cast_variant_paid` is also written post-resolution for other
            // variants (Sneak/Evoke/Escape), but those have no ETB-replacement
            // gate; web-slinging does, so its write must precede `replace_event`.
            if let CastingVariant::WebSlinging { .. } = casting_variant {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::WebSlinging,
                        state.turn_number,
                    ));
                }
            }

            let convoked_creatures = state
                .objects
                .get(&entry.id)
                .map(|obj| obj.convoked_creatures.clone())
                .unwrap_or_default();
            // CR 702.33d + CR 400.7 + CR 400.7d: Capture the authoritative kicker
            // payments BEFORE `move_to_zone` clears `kickers_paid` on the new
            // battlefield object (CR 400.7 new-object rule, applied by
            // `reset_for_battlefield_entry`). The resolving spell's `SpellContext`
            // is authoritative when present; placeholder permanent spells (vanilla
            // / ETB-only creatures with no on-resolve Spell ability) have
            // `ability == None`, so fall back to the stack object's stamped value.
            let kickers_paid: Vec<crate::types::ability::KickerVariant> = ability
                .as_ref()
                .map(|a| a.context.kickers_paid.clone())
                .unwrap_or_else(|| {
                    state
                        .objects
                        .get(&entry.id)
                        .map(|o| o.kickers_paid.clone())
                        .unwrap_or_default()
                });
            let additional_cost_payment_count = ability
                .as_ref()
                .map(|a| a.context.additional_cost_payment_count)
                .unwrap_or_else(|| {
                    state
                        .objects
                        .get(&entry.id)
                        .map(|o| o.additional_cost_payment_count)
                        .unwrap_or_default()
                });
            let cast_timing_permission = state
                .objects
                .get(&entry.id)
                .and_then(|obj| obj.cast_timing_permission.map(|(permission, _)| permission));

            match super::replacement::replace_event(state, proposed, events) {
                super::replacement::ReplacementResult::Execute(event) => {
                    if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                        object_id,
                        to,
                        enter_tapped,
                        enter_with_counters,
                        controller_override,
                        enter_transformed,
                        ..
                    } = event
                    {
                        // CR 608.3 + 608.2c: Stack-residency guard — see
                        // `spell_still_on_stack`. If `execute_effect` already
                        // moved the spell off the stack via a self-targeted
                        // sub-ability (e.g., a permanent spell whose
                        // resolution self-exiles), skip the default
                        // Stack→Battlefield move and the ETB bookkeeping that
                        // would attach to it. The spell is in its
                        // self-chosen destination — applying ETB-tapped /
                        // counter / transform state to a non-battlefield
                        // zone is meaningless and would corrupt the object.
                        if spell_still_on_stack(state, object_id) {
                            zones::move_to_zone(state, object_id, to, events);
                            if let Some(obj) = state.objects.get_mut(&object_id) {
                                if enter_tapped.resolve(false) {
                                    obj.tapped = true;
                                }
                                if let Some(new_controller) = controller_override {
                                    obj.controller = new_controller;
                                }
                            }
                            // CR 614.1c: Apply counters from replacement pipeline
                            // (e.g., saga lore counters per CR 714.3a, planeswalker
                            // intrinsic loyalty per CR 306.5b, battle intrinsic
                            // defense per CR 310.4b).
                            super::engine_replacement::apply_etb_counters(
                                state,
                                object_id,
                                &enter_with_counters,
                                events,
                            );
                            // CR 712.14a + CR 310.11b: Apply transformation if entering
                            // transformed (propagated from ExileWithAltCost permission).
                            if enter_transformed && to == Zone::Battlefield {
                                if let Some(obj) = state.objects.get(&object_id) {
                                    if obj.back_face.is_some() && !obj.transformed {
                                        let _ = super::transform::transform_permanent(
                                            state, object_id, events,
                                        );
                                    }
                                }
                            }
                            // CR 614.1c: Apply pending ETB counters from delayed triggers
                            // (e.g., "that creature enters with an additional +1/+1 counter").
                            let pending: Vec<_> = state
                                .pending_etb_counters
                                .iter()
                                .filter(|(oid, _, _)| *oid == object_id)
                                .map(|(_, ct, n)| (ct.clone(), *n))
                                .collect();
                            if !pending.is_empty() {
                                super::engine_replacement::apply_etb_counters(
                                    state, object_id, &pending, events,
                                );
                                state
                                    .pending_etb_counters
                                    .retain(|(oid, _, _)| *oid != object_id);
                            }
                        }
                    }
                    // CR 603.4: Propagate cast_from_zone to the permanent so ETB triggers
                    // can evaluate conditions like "if you cast it from your hand".
                    // When ability is present, use its context; otherwise the object
                    // already has cast_from_zone set during finalize_cast_to_stack.
                    if spell_in_zone(state, entry.id, Zone::Battlefield) {
                        if let Some(obj) = state.objects.get_mut(&entry.id) {
                            if let Some(ref ability) = ability {
                                obj.cast_from_zone = ability.context.cast_from_zone;
                            }
                            if let Some(permission) = cast_timing_permission {
                                obj.cast_timing_permission = Some((permission, state.turn_number));
                            }
                            obj.convoked_creatures = convoked_creatures;
                            // CR 702.33d + CR 400.7d: Restore kicker payments onto the
                            // resulting permanent so post-resolution gates
                            // (`ReplacementCondition::CastViaKicker` and ETB
                            // `AbilityCondition::AdditionalCostPaid` on triggered
                            // abilities) can evaluate. `move_to_zone` cleared
                            // `kickers_paid` per CR 400.7 (new object on zone change);
                            // CR 400.7d permits an ability of the permanent to
                            // reference costs paid to cast the spell it became. This
                            // restore is unconditional — mirroring `convoked_creatures`
                            // — because placeholder permanent spells have
                            // `ability == None` and would otherwise lose the data.
                            obj.kickers_paid = kickers_paid;
                            obj.additional_cost_payment_count = additional_cost_payment_count;
                        }
                        if let Some(exiled_id) = ability
                            .as_ref()
                            .and_then(|ability| ability.cost_paid_object.as_ref())
                            .map(|snapshot| snapshot.object_id)
                            .filter(|exiled_id| {
                                state
                                    .objects
                                    .get(exiled_id)
                                    .is_some_and(|obj| obj.zone == Zone::Exile)
                            })
                        {
                            if !state.exile_links.iter().any(|link| {
                                link.source_id == entry.id && link.exiled_id == exiled_id
                            }) {
                                state.exile_links.push(ExileLink {
                                    exiled_id,
                                    source_id: entry.id,
                                    kind: ExileLinkKind::UntilSourceLeaves {
                                        return_zone: Zone::Hand,
                                    },
                                });
                            }
                        }
                        super::room::unlock_door_designation(
                            state,
                            entry.id,
                            entry.controller,
                            crate::game::game_object::RoomDoor::Left,
                            events,
                        );
                    }
                    // CR 614.12a: Drain mandatory replacement post-effects (e.g., the
                    // Siege protector / Tribute opponent-choice prompt that was stashed
                    // by `apply_single_replacement` while resolving this ZoneChange).
                    // Sets `state.waiting_for` to the resulting prompt, if any — the
                    // caller's post-stack resolution checks waiting_for before returning
                    // priority. Without this drain the choice would be silently dropped.
                    if state.post_replacement_continuation.is_some() {
                        state.post_replacement_source = None;
                        let _ = super::engine_replacement::apply_pending_post_replacement_effect(
                            state,
                            Some(entry.id),
                            None,
                            Some(crate::types::replacements::ReplacementEvent::Moved),
                            events,
                        );
                    }
                }
                super::replacement::ReplacementResult::Prevented => {
                    // CR 608.3e: Permanent spell's ETB was fully prevented —
                    // the card goes to owner's graveyard instead. Stack-residency
                    // guard (`spell_still_on_stack`): if the spell already
                    // self-moved during `execute_effect` (e.g., a permanent
                    // whose resolution self-exiles before its ETB would have
                    // resolved), skip the prevented-ETB graveyard fallback so
                    // the self-chosen destination is honored (issue #323
                    // class).
                    move_prevented_permanent_spell_to_graveyard_if_still_on_stack(
                        state, entry.id, events,
                    );
                }
                super::replacement::ReplacementResult::NeedsChoice(player) => {
                    // A replacement needs player choice (e.g., Clone "enter as a copy").
                    // Store context so handle_replacement_choice can complete post-resolution.
                    let cast_from_zone = ability
                        .as_ref()
                        .and_then(|a| a.context.cast_from_zone)
                        .or_else(|| state.objects.get(&entry.id).and_then(|o| o.cast_from_zone));
                    // CR 702.33d + CR 400.7d: Use the authoritative kicker payments
                    // (resolving spell's `SpellContext` when present, else the stack
                    // object's stamped value) so placeholder permanent spells with
                    // `ability == None` are not silently de-kicked when a replacement
                    // needs a player choice. `engine_replacement` restores this onto
                    // the permanent unconditionally after the choice resolves.
                    let kickers_paid = ability
                        .as_ref()
                        .map(|a| a.context.kickers_paid.clone())
                        .unwrap_or_else(|| {
                            state
                                .objects
                                .get(&entry.id)
                                .map(|o| o.kickers_paid.clone())
                                .unwrap_or_default()
                        });
                    let additional_cost_payment_count = ability
                        .as_ref()
                        .map(|a| a.context.additional_cost_payment_count)
                        .unwrap_or_else(|| {
                            state
                                .objects
                                .get(&entry.id)
                                .map(|o| o.additional_cost_payment_count)
                                .unwrap_or_default()
                        });
                    state.pending_spell_resolution =
                        Some(crate::types::game_state::PendingSpellResolution {
                            object_id: entry.id,
                            controller: entry.controller,
                            casting_variant,
                            cast_from_zone,
                            cast_timing_permission,
                            spell_targets: spell_targets.clone(),
                            actual_mana_spent,
                            kickers_paid,
                            additional_cost_payment_count,
                            convoked_creatures,
                        });
                    state.waiting_for =
                        super::replacement::replacement_choice_waiting_for(player, state);
                    // Emit StackResolved now — the spell has left the stack even though
                    // the replacement choice is pending.
                    events.push(GameEvent::StackResolved {
                        object_id: entry.id,
                    });
                    state.current_trigger_event = None;
                    state.current_trigger_events.clear();
                    state.current_trigger_match_count = None;
                    return;
                }
            }
        } else {
            // CR 608.2n: "As the final part of an instant or sorcery spell's
            // resolution, the spell is put into its owner's graveyard."
            // Stack-residency guard (`spell_still_on_stack`): if the spell's
            // own instructions already moved it off the Stack (e.g., Treasured
            // Find / Arc Blade — "Exile ~", or any sub-ability that targets
            // the source via `SelfRef`), the post-resolution default move must
            // be skipped — otherwise the spell card travels exile→graveyard
            // and undoes its own self-exile clause (issue #323).
            if spell_still_on_stack(state, entry.id) {
                zones::move_to_zone(state, entry.id, dest, events);
            }
        }

        // CR 715.4 / CR 720.4: Outside the stack, Adventure-family cards have
        // their normal characteristics.
        if matches!(
            casting_variant,
            CastingVariant::Adventure | CastingVariant::Omen
        ) {
            restore_alternative_spell_normal_face(state, entry.id);
        }

        // CR 715.3d: When an Adventure spell resolves to exile, grant
        // AdventureCreature permission so it can be cast from exile.
        if casting_variant == CastingVariant::Adventure {
            if let Some(obj) = state.objects.get_mut(&entry.id) {
                obj.casting_permissions
                    .push(crate::types::ability::CastingPermission::AdventureCreature);
            }
        }
        if casting_variant == CastingVariant::Omen {
            if let Some(owner) = state
                .objects
                .get(&entry.id)
                .filter(|obj| obj.zone == Zone::Library)
                .map(|obj| obj.owner)
            {
                effects::change_zone::shuffle_library(state, owner, events);
            }
        }

        // CR 303.4f: Aura resolving to battlefield attaches to its target.
        if spell_in_zone(state, entry.id, Zone::Battlefield) {
            let is_aura = state
                .objects
                .get(&entry.id)
                .map(|obj| obj.card_types.subtypes.iter().any(|s| s == "Aura"))
                .unwrap_or(false);
            if is_aura {
                match spell_targets.first() {
                    // CR 303.4f + CR 608.2b: Object Aura — verify the target is
                    // still on the battlefield (last-known-information check); a
                    // gone target leaves the Aura unattached and SBA
                    // (CR 704.5m) cleans it up at the next checkpoint.
                    Some(crate::types::ability::TargetRef::Object(target_id))
                        if state.battlefield.contains(target_id) =>
                    {
                        effects::attach::attach_to(state, entry.id, *target_id);
                    }
                    Some(crate::types::ability::TargetRef::Object(_)) => {
                        // Target left the battlefield — SBA cleanup follows.
                    }
                    // CR 303.4f + CR 702.5d: Player Aura (Curse cycle, Faith's
                    // Fetters-class). Validity check is "player still in game"
                    // — `attach_to_player` makes no liveness check itself, but
                    // `check_unattached_auras` (CR 303.4c) will detach + grave
                    // a Curse whose enchanted player has left the game.
                    Some(crate::types::ability::TargetRef::Player(player_id)) => {
                        effects::attach::attach_to_player(state, entry.id, *player_id);
                    }
                    None => {
                        // CR 303.4g: An Aura entering the battlefield with no
                        // legal target goes to its owner's graveyard. The SBA
                        // path catches this on the next pass.
                    }
                }
            }

            // CR 702.185a: Warp — when a permanent cast via Warp resolves to the battlefield,
            // create a delayed trigger to exile it at end step with WarpExile permission.
            // Only triggers on the initial Warp cast (CastingVariant::Warp), NOT on re-casts
            // from exile (which use CastingVariant::Normal and stay permanently).
            if casting_variant == CastingVariant::Warp {
                let has_warp = state.objects.get(&entry.id).is_some_and(|obj| {
                    obj.keywords
                        .iter()
                        .any(|k| matches!(k, crate::types::keywords::Keyword::Warp(_)))
                });
                if has_warp {
                    create_warp_delayed_trigger(state, entry.id, entry.controller);
                }
            }

            // CR 702.190b: Sneak-cast permanent enters tapped (already seeded on
            // the ZoneChange replacement) AND attacking the same defender as the
            // returned creature. Placement is `Some` only for permanent spells;
            // non-permanent Sneak casts (instants/sorceries) resolve normally.
            // Also tag `cast_variant_paid` so the `CastVariantPaid { variant:
            // Sneak }` trigger/ability condition fires on resolved Sneak casts
            // regardless of card type.
            if let CastingVariant::Sneak { placement, .. } = casting_variant {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Sneak,
                        state.turn_number,
                    ));
                }
                if let Some(p) = placement {
                    super::combat::place_attacking_alongside(
                        state,
                        entry.id,
                        p.defender,
                        p.attack_target,
                        events,
                    );
                }
            }

            // CR 702.188a: Web-slinging's `cast_variant_paid` tag is written
            // before `replace_event` above (so the ETB-replacement gate can
            // read it) — no post-resolution write is needed here.

            // CR 702.74a: Evoke-cast permanent gets the `cast_variant_paid` tag
            // so the synthesized intervening-if ETB sacrifice trigger fires.
            if casting_variant == CastingVariant::Evoke {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Evoke,
                        state.turn_number,
                    ));
                }
            }

            // CR 702.103a + CR 702.103b: Bestow-cast permanent gets the
            // `cast_variant_paid` tag so future "if its bestow cost was paid"
            // triggers/conditions can evaluate against the resolved permanent.
            // Tag is set whether the bestow form persisted (legal target →
            // Aura attached) or was reverted at resolution (CR 702.103e
            // illegal-target → resolved as creature) — the audit trail is the
            // *cost* paid, not the form at ETB.
            if casting_variant == CastingVariant::Bestow {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Bestow,
                        state.turn_number,
                    ));
                }
            }

            // CR 702.138b: Escape-cast permanent is tagged so the "unless it
            // escaped" intervening-if on Phlage, Titan of Fire's Fury (and any
            // future escape-gated ETB trigger) can distinguish escape casts
            // from hard-casts and reanimation. Per CR 702.138b: "A spell or
            // permanent 'escaped' if that spell ... was cast from a graveyard
            // with an escape ability."
            if casting_variant == CastingVariant::Escape {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Escape,
                        state.turn_number,
                    ));
                }
            }

            // CR 702.62a: Suspend-cast permanent gets the `cast_variant_paid`
            // tag for symmetry with Evoke / Sneak (no synthesized trigger reads
            // it today, but it preserves the audit trail). Additionally, when
            // the resolving spell was a creature, install a transient
            // continuous "has haste" effect that lapses the moment another
            // player gains control of the permanent
            // (CR 702.62a final sentence: "If you cast a creature spell this
            // way, it gains haste until you lose control of the spell or the
            // permanent it becomes."). The layer-6 keyword grant is scoped to
            // the resolving permanent via `TargetFilter::SpecificObject` and
            // gated by `Duration::ForAsLongAs { SourceControllerEquals }` —
            // a Threaten-style control swap flips the predicate false and the
            // static is gathered out of layer evaluation.
            if casting_variant == CastingVariant::Suspend {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Suspend,
                        state.turn_number,
                    ));
                }

                let is_creature = state
                    .objects
                    .get(&entry.id)
                    .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Creature));
                if is_creature {
                    let resolution_controller = entry.controller;
                    let suspended_id = entry.id;
                    state.add_transient_continuous_effect(
                        suspended_id,
                        resolution_controller,
                        Duration::ForAsLongAs {
                            condition:
                                crate::types::ability::StaticCondition::SourceControllerEquals {
                                    player: resolution_controller,
                                },
                        },
                        crate::types::ability::TargetFilter::SpecificObject { id: suspended_id },
                        vec![ContinuousModification::AddKeyword {
                            keyword: crate::types::keywords::Keyword::Haste,
                        }],
                        None,
                    );
                }
            }
        }
    }
    // Activated abilities: source stays where it is, no zone movement

    // CR 603.7c: Clear trigger event context after resolution completes.
    state.current_trigger_event = None;
    state.current_trigger_events.clear();
    state.current_trigger_match_count = None;

    events.push(GameEvent::StackResolved {
        object_id: entry.id,
    });
}

/// CR 113.3b + CR 113.7a: Resolve an activated keyword ability from the stack.
///
/// The cost has already been paid at announcement. Resolution applies the
/// keyword's effect against last-known information — if a participating
/// object has left its expected zone between announcement and resolution,
/// the effect is either skipped or applied using the snapshot carried on
/// the `KeywordAction` payload (e.g. `Station::snapshot_power`).
fn resolve_keyword_action(
    state: &mut GameState,
    action: KeywordAction,
    events: &mut Vec<GameEvent>,
) {
    match action {
        // CR 702.6a: Attach source Equipment to target creature. If either
        // object has left the battlefield by resolution, the effect does nothing
        // (CR 608.2b — illegal-target check on resolution).
        KeywordAction::Equip {
            equipment_id,
            target_creature_id,
        } => {
            let still_valid = state
                .objects
                .get(&equipment_id)
                .is_some_and(|e| e.zone == Zone::Battlefield)
                && state.objects.get(&target_creature_id).is_some_and(|t| {
                    t.zone == Zone::Battlefield
                        && t.card_types.core_types.contains(&CoreType::Creature)
                });
            if still_valid {
                if let Some(old_target) =
                    effects::attach::attach_to(state, equipment_id, target_creature_id)
                {
                    events.push(GameEvent::Unattached {
                        attachment_id: equipment_id,
                        old_target,
                    });
                }
            }
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Equip,
                source_id: equipment_id,
            });
        }
        // CR 702.122a: This permanent becomes an artifact creature UEOT.
        KeywordAction::Crew {
            vehicle_id,
            paid_creature_ids,
        } => {
            if let Some(v) = state.objects.get(&vehicle_id) {
                if v.zone == Zone::Battlefield {
                    let controller = v.controller;
                    state.add_transient_continuous_effect(
                        vehicle_id,
                        controller,
                        Duration::UntilEndOfTurn,
                        TargetFilter::SpecificObject { id: vehicle_id },
                        vec![ContinuousModification::AddType {
                            core_type: CoreType::Creature,
                        }],
                        None,
                    );
                }
            }
            events.push(GameEvent::VehicleCrewed {
                vehicle_id,
                creatures: paid_creature_ids,
            });
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Crew,
                source_id: vehicle_id,
            });
        }
        // CR 702.171a: This permanent becomes saddled UEOT.
        // CR 702.171b: The saddled designation is stored on the GameObject and
        // cleared at end of turn or when it leaves the battlefield.
        KeywordAction::Saddle {
            mount_id,
            paid_creature_ids,
        } => {
            if let Some(mount) = state.objects.get_mut(&mount_id) {
                if mount.zone == Zone::Battlefield {
                    mount.is_saddled = true;
                }
            }
            events.push(GameEvent::Saddled {
                mount_id,
                creatures: paid_creature_ids,
            });
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Saddle,
                source_id: mount_id,
            });
        }
        // CR 702.184a: Put charge counters equal to the tapped creature's power.
        // The power reading was snapshot at announcement (CR 113.7a) so this is
        // safe even if the paid creature has since left the battlefield.
        KeywordAction::Station {
            spacecraft_id,
            paid_creature_id,
            snapshot_power,
        } => {
            let counters_added = snapshot_power.max(0) as u32;
            let spacecraft_controller = state
                .objects
                .get(&spacecraft_id)
                .filter(|sc| sc.zone == Zone::Battlefield)
                .map(|sc| sc.controller);
            if let (Some(controller), true) = (spacecraft_controller, counters_added > 0) {
                effects::counters::add_counter_with_replacement(
                    state,
                    controller,
                    spacecraft_id,
                    CounterType::Generic("charge".to_string()),
                    counters_added,
                    events,
                );
            }
            events.push(GameEvent::Stationed {
                spacecraft_id,
                creature_id: paid_creature_id,
                counters_added,
            });
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Station,
                source_id: spacecraft_id,
            });
        }
    }
}

// ── Tier 3: true batch-resolution of identical token-creating triggers ────
//
// `resolve_next` wraps `resolve_top`. When the top of the stack begins a
// contiguous run of provably-batch-safe identical triggered abilities, it
// resolves the whole run in one step that applies the effect N times — the
// same observable state and (coalesced) event sequence as one-by-one. Any
// uncertainty falls back to the unchanged `resolve_top`. Three layers gate
// eligibility: Layer A (run-identity, `BatchRunKey`), Layer B (handler purity,
// `effects::try_resolve_batch`), Layer C (observer-order-invariance,
// `observers_are_batch_safe`). See the plan trace in `effects/mod.rs`.

/// Sentinel object id used only to build Layer C probe events. `keys_from_event`
/// reads only `record.core_types`/`to` (ETB keys) and the `TokenCreated` variant
/// tag — never the `object_id` — so a sentinel is sound (§2.3 PROBE_ID note).
const PROBE_ID: ObjectId = ObjectId(u64::MAX);

/// CR 608.2: Resolve the next stack object, collapsing a batch-safe run when
/// one begins at the top. Returns the number of stack entries consumed
/// (≥ 1) so the caller can correct the auto-pass baseline (§7.2).
pub fn resolve_next(state: &mut GameState, events: &mut Vec<GameEvent>) -> u32 {
    // CR 603.3c/d: never collapse while the top entry is mid-construction.
    let pending_top = state
        .pending_trigger_entry
        .is_some_and(|pending| state.stack.back().map(|e| e.id) == Some(pending));
    if !pending_top {
        if let Some(run_len) = batch_run_len(state) {
            if run_len >= 2 {
                // Layer B FIRST: per-handler purity produces the resolved token
                // spec(s) the Layer C probe needs (HIGH-1) and applies the
                // §2.2a/§2.3a/§3.4 gates internally.
                let ability = state.stack.back().and_then(|e| e.ability()).cloned();
                if let Some(ability) = ability {
                    if let Some(plan) = effects::try_resolve_batch(state, &ability, run_len) {
                        // Layer C: lazily refresh the index sentinel (mirrors the
                        // consult site at triggers.rs:790) before the read-only probe.
                        if state.trigger_index.by_key.is_empty()
                            && state.trigger_index.unclassified.is_empty()
                            && !state.battlefield.is_empty()
                        {
                            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
                        }
                        if observers_are_batch_safe(state, &plan) {
                            return resolve_batched(state, &plan, &ability, events);
                        }
                    }
                }
            }
        }
    }
    resolve_top(state, events);
    1
}

/// CR 608.2: Apply a proven-safe batch. The per-resolution handler body runs
/// `consumed` times (§5.2a — no count-fusion in v1), with the pipeline
/// checkpoint hoisted to once-after by the caller. Per-entry `StackResolved`
/// events are emitted for every consumed entry (§5.4) so the frontend's
/// per-entry fade still works. Returns the number of entries consumed.
fn resolve_batched(
    state: &mut GameState,
    plan: &effects::BatchPlan,
    ability: &crate::types::ability::ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> u32 {
    let consumed = plan.consumed();
    state.resolving_stack_entry = None;

    // Pop the run's entries (resolution order is back-to-front), cleaning the
    // per-entry side tables exactly as `resolve_top` does for a single entry.
    let mut popped = Vec::with_capacity(consumed as usize);
    for _ in 0..consumed {
        match state.stack.pop_back() {
            Some(entry) => {
                state.stack_paid_facts.remove(&entry.id);
                state.stack_trigger_event_batches.remove(&entry.id);
                popped.push(entry);
            }
            None => break,
        }
    }

    // CR 603.7c: Set the trigger event context once from the (identical) top
    // entry — all popped entries are deep-equal by `BatchRunKey`, so a single
    // set/clear is equivalent to N idempotent sequential set/clear cycles.
    if let Some(top) = popped.first() {
        if let crate::types::game_state::StackEntryKind::TriggeredAbility {
            trigger_event: Some(te),
            subject_match_count,
            ..
        } = &top.kind
        {
            state.current_trigger_event = Some(te.clone());
            state.current_trigger_events = vec![te.clone()];
            state.current_trigger_match_count = *subject_match_count;
        }
    }

    // CR 608.2: Apply the effect N times through the existing per-resolution body.
    plan.execute(state, ability, events);

    // CR 603.7c: Clear trigger context after resolution completes.
    state.current_trigger_event = None;
    state.current_trigger_events.clear();
    state.current_trigger_match_count = None;

    // §5.4: one StackResolved per consumed entry.
    for entry in &popped {
        events.push(GameEvent::StackResolved {
            object_id: entry.id,
        });
    }

    popped.len() as u32
}

/// CR 603.2 + CR 603.3 + CR 603.6a: Layer C — battlefield-wide
/// observer-order-invariance gate. A batched run is order-invariant iff NO
/// battlefield trigger fans out on the token-ETB events the batch will emit.
/// Build the REAL `ZoneChanged` + `TokenCreated` events one produced token
/// emits (from the resolved spec's true characteristics) and route each through
/// the public `candidates_for_event` — the same `keys_from_event` path the real
/// events take downstream, with NO hand-picked key set. If ANY observer is
/// registered for those events — including one on the run's own source (HIGH-2:
/// a source carrying a second observer trigger keyed on the produced token's
/// ETB/TokenCreated must NOT be excluded; doing so would skip the per-trigger
/// priority interleaving CR 603.3 requires) — sequential resolution interleaves
/// it per-token (CR 603.3 topmost-on-stack), so the batch ("all tokens, then
/// all observers") may diverge. Refuse, fall back per-entry. The §2.2a
/// emits-exactly gate makes this two-event probe complete by construction for
/// ALL observer axes.
fn observers_are_batch_safe(state: &GameState, plan: &effects::BatchPlan) -> bool {
    for spec in plan.produced_token_specs() {
        let record = zone_change_record_from_spec(spec);
        let zc = GameEvent::ZoneChanged {
            object_id: PROBE_ID,
            from: None,
            to: Zone::Battlefield,
            record: Box::new(record),
        };
        let tc = GameEvent::TokenCreated {
            object_id: PROBE_ID,
            name: spec.characteristics.display_name.clone(),
        };
        for ev in [&zc, &tc] {
            // unclassified ∪ buckets matching keys_from_event(ev). The
            // unclassified bucket (Always/Immediate/dynamic/synthetic-keyword)
            // is unconditionally included → any catch-all observer forces refuse.
            // CR 603.3: any registered observer (including the run's own source)
            // forces sequential resolution so priority interleaves per-token.
            let candidates = crate::game::trigger_index::candidates_for_event(state, ev);
            if !candidates.is_empty() {
                return false;
            }
        }
    }
    true
}

/// CR 603.6a + CR 603.10: Build the faithful `ZoneChangeRecord` a produced
/// token emits, from the resolved `TokenSpec` characteristics. `keys_from_event`
/// reads only `core_types`/`to` for ETB keys, so the record's `core_types`
/// drives the entire probe key set (mirrors `snapshot_for_zone_change`).
fn zone_change_record_from_spec(
    spec: &crate::types::proposed_event::TokenSpec,
) -> crate::types::game_state::ZoneChangeRecord {
    let ch = &spec.characteristics;
    crate::types::game_state::ZoneChangeRecord {
        object_id: PROBE_ID,
        name: ch.display_name.clone(),
        core_types: ch.core_types.clone(),
        subtypes: ch.subtypes.clone(),
        supertypes: ch.supertypes.clone(),
        keywords: ch.keywords.clone(),
        power: ch.power,
        toughness: ch.toughness,
        base_power: ch.power,
        base_toughness: ch.toughness,
        colors: ch.colors.clone(),
        mana_value: 0,
        controller: spec.controller,
        owner: spec.controller,
        from_zone: None,
        to_zone: Zone::Battlefield,
        attachments: Vec::new(),
        linked_exile_snapshot: Vec::new(),
        is_token: true,
        combat_status: Default::default(),
    }
}

/// Resolution-grade run key (stricter than the display `StackGroupKey`, §4.1).
/// Two adjacent entries join a run iff every field is equal AND the entry is an
/// untargeted `TriggeredAbility` (Layer A). Keyed on `source_id` + deep-equal
/// `ResolvedAbility` (not display `source_name`), with the flattened target
/// vector required empty (CR 608.2b).
#[derive(PartialEq)]
struct BatchRunKey<'a> {
    controller: PlayerId,
    source_id: ObjectId,
    ability: &'a ResolvedAbility,
    description: Option<&'a str>,
    paid: Option<&'a StackPaidSnapshot>,
    trigger_event: Option<&'a GameEvent>,
}

/// Build the run key for an entry, or `None` if the entry is not a candidate
/// for batch-resolution (Layer A.1/A.4/A.5: must be an untargeted
/// `TriggeredAbility` with no entry-level intervening-if condition).
///
/// No-wildcard discipline: every field of the `TriggeredAbility` variant is
/// destructured explicitly (no `..`) so each is consciously dispositioned —
/// the same exhaustiveness the codebase mandates for match arms, applied to
/// struct destructuring. Field-by-field audit:
/// - `source_id`   — IN KEY (run-identity: only one source's run collapses).
/// - `ability`     — IN KEY (deep-equal `ResolvedAbility`: identical effect).
/// - `condition`   — RESOLUTION-RELEVANT, NOT in key. CR 603.4: the entry-level
///   intervening-if is rechecked per entry at resolution (`resolve_top`
///   stack.rs:120-140) and the effect is skipped once the condition flips. The
///   batch path applies the effect N times WITHOUT a per-entry recheck, so a
///   run carrying an order-sensitive intervening-if (one the run's own tokens
///   could move across its threshold) would diverge from sequential. We do NOT
///   attempt to prove invariance in v1: any `condition.is_some()` makes the
///   entry NON-batchable, forcing it into a singleton run that falls back to
///   the `resolve_top` path which rechecks correctly. Conservative refuse.
/// - `trigger_event` — IN KEY (event context drives `EventContextAmount`, etc.;
///   differing context must not collapse).
/// - `description` — IN KEY (distinguishes triggers from the same source).
/// - `source_name` — RESOLUTION-IRRELEVANT: a display-only pre-resolved name
///   (game_state.rs:3493-3500) the frontend renders; it derives from
///   `source_id` (already in key) and is never read during resolution. Not in
///   key by design.
/// - `subject_match_count` — RESOLUTION-RELEVANT but PROVABLY EQUAL across a
///   run: it is the CR 603.2c filtered subject count from the firing event
///   batch. `resolve_batched` lifts it into resolution scope from the run's top
///   entry (stack.rs:1135-1145), and `trigger_event` (which carries the firing
///   event) is already in the key — two entries with equal `trigger_event` and
///   equal deep `ability` carry the same batched subject count. It is therefore
///   redundant to key on (would never break a run the other fields kept
///   together) and is correctly applied from the top entry in the batch path.
fn batch_run_key<'a>(state: &'a GameState, entry: &'a StackEntry) -> Option<BatchRunKey<'a>> {
    let StackEntryKind::TriggeredAbility {
        source_id,
        ability,
        condition,
        trigger_event,
        description,
        source_name: _,
        subject_match_count: _,
    } = &entry.kind
    else {
        return None;
    };
    // CR 608.2b: untargeted-only — targets re-check legality per resolution.
    if !flatten_targets_in_chain(ability).is_empty() {
        return None;
    }
    // CR 603.4 (verified docs/MagicCompRules.txt:2588): an entry-level
    // intervening-if is rechecked per entry at resolution and skips the effect
    // once it flips. The batch path does not recheck per entry, so refuse to
    // group any entry carrying one — it becomes a singleton run and falls back
    // to the `resolve_top` path that rechecks correctly.
    if condition.is_some() {
        return None;
    }
    Some(BatchRunKey {
        controller: entry.controller,
        source_id: *source_id,
        ability,
        description: description.as_deref(),
        paid: state.stack_paid_facts.get(&entry.id),
        trigger_event: trigger_event.as_ref(),
    })
}

/// CR 405.1: Length of the maximal contiguous run of batch-key-equal entries
/// starting at the TOP of the stack (resolution order is back-to-front).
/// Returns `None` when the top entry is not a batch candidate. Contiguous-only:
/// a non-adjacent look-alike across a gap must resolve in true stack order.
fn batch_run_len(state: &GameState) -> Option<u32> {
    let top = state.stack.back()?;
    let top_key = batch_run_key(state, top)?;
    let mut len = 1u32;
    // Walk downward from just below the top.
    for entry in state.stack.iter().rev().skip(1) {
        match batch_run_key(state, entry) {
            Some(key) if key == top_key => len += 1,
            _ => break,
        }
    }
    Some(len)
}

fn execute_effect(
    state: &mut GameState,
    ability: &crate::types::ability::ResolvedAbility,
    events: &mut Vec<GameEvent>,
) {
    // Skip unimplemented effects (logged elsewhere as warnings)
    if matches!(
        ability.effect,
        crate::types::ability::Effect::Unimplemented { .. }
    ) {
        return;
    }
    // Use resolve_ability_chain to support SubAbility/Execute chaining
    let _ = effects::resolve_ability_chain(state, ability, events, 0);
}

pub fn stack_is_empty(state: &GameState) -> bool {
    state.stack.is_empty()
}

// ── Display-only stack pressure + grouping ──────────────────────────────
//
// These are UX pacing/presentation primitives, not a rules concept. No CR
// citation — the Comprehensive Rules say nothing about how quickly the
// client should animate stack resolution or whether identical triggers
// should be collapsed visually. Owned by the engine so every consumer
// (browser, desktop, server) shares one authoritative threshold and one
// authoritative grouping predicate. Frontend maps StackPressure → animation
// multiplier; it never decides what "identical" means or when to skip a
// mount animation.

/// Size at which the stack transitions out of "Normal" animation pacing.
pub const STACK_PRESSURE_ELEVATED: usize = 10;
/// Size at which stack animation must be noticeably faster.
pub const STACK_PRESSURE_RAPID: usize = 30;
/// Size at which per-entry mount animation should be skipped entirely.
pub const STACK_PRESSURE_INSTANT: usize = 100;

/// Display-only pacing bucket for stack resolution animations. Not a rules
/// concept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StackPressure {
    Normal,
    Elevated,
    Rapid,
    Instant,
}

/// Compute the current stack pressure. Just-in-time — never stored on
/// GameState per CLAUDE.md's "only compute when needed" guideline.
pub fn stack_pressure(state: &GameState) -> StackPressure {
    match state.stack.len() {
        n if n >= STACK_PRESSURE_INSTANT => StackPressure::Instant,
        n if n >= STACK_PRESSURE_RAPID => StackPressure::Rapid,
        n if n >= STACK_PRESSURE_ELEVATED => StackPressure::Elevated,
        _ => StackPressure::Normal,
    }
}

/// A coalesced group of "visually identical" stack entries. The frontend
/// renders one badge per group with `count` as a ×N suffix on the
/// representative card.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StackDisplayGroup {
    /// The first entry in the group — frontend uses its card image/name.
    pub representative: ObjectId,
    /// Number of coalesced entries (always ≥ 1).
    pub count: u32,
    /// All coalesced entry ids, in stack order. Used by UI animations that
    /// need to key per-entry (e.g., fade each out in turn on resolution).
    pub member_ids: Vec<ObjectId>,
}

/// Produce a display-grouped view of the stack. Adjacent entries with the
/// same (source card name, kind discriminant, trigger description) are
/// coalesced. Non-adjacent look-alikes stay separate — coalescing only
/// adjacent entries preserves the actual resolution order for cases like
/// stacked triggers from different sources interleaving.
pub fn stack_display_groups(state: &GameState) -> Vec<StackDisplayGroup> {
    let mut out: Vec<StackDisplayGroup> = Vec::new();
    // Track the previous entry's key alongside the output vector so we can
    // decide "merge or push" in O(1) per entry instead of re-scanning the
    // stack to look up the representative each iteration.
    let mut last_key: Option<StackGroupKey> = None;
    for entry in &state.stack {
        // KeywordAction entries (Equip/Crew/Station/Saddle) carry their
        // target inside the enum variant, not via ResolvedAbility, so the
        // target-aware signature cannot see it. Rather than reach into
        // every keyword payload just to discriminate two consecutive
        // keyword activations (a vanishingly rare scenario), we opt them
        // out of coalescing: always push a fresh group and clear
        // `last_key` so a following non-keyword entry also starts fresh.
        if matches!(entry.kind, StackEntryKind::KeywordAction { .. }) {
            out.push(StackDisplayGroup {
                representative: entry.id,
                count: 1,
                member_ids: vec![entry.id],
            });
            last_key = None;
            continue;
        }
        let key = group_key(state, entry);
        if last_key.as_ref() == Some(&key) {
            let last = out.last_mut().unwrap();
            last.count += 1;
            last.member_ids.push(entry.id);
        } else {
            out.push(StackDisplayGroup {
                representative: entry.id,
                count: 1,
                member_ids: vec![entry.id],
            });
            last_key = Some(key);
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StackGroupKey {
    source_name: String,
    tag: &'static str,
    description: Option<String>,
    targets: Vec<TargetRef>,
    paid: Option<StackPaidSnapshot>,
    trigger_context: Vec<String>,
}

/// Grouping signature for `stack_display_groups`. Two entries coalesce iff
/// their signatures are equal. Includes the resolved target vector so
/// visually-identical triggers that fire against different targets (e.g.
/// N copies of "target player loses 1 life" picking different players)
/// remain separate — coalescing them would misrepresent the resolution.
fn group_key(state: &GameState, entry: &StackEntry) -> StackGroupKey {
    let source_name = state
        .objects
        .get(&entry.source_id)
        .map(|o| o.name.clone())
        .unwrap_or_default();
    let (tag, description) = match &entry.kind {
        StackEntryKind::Spell { .. } => ("spell", None),
        StackEntryKind::ActivatedAbility { .. } => ("activated", None),
        StackEntryKind::TriggeredAbility { description, .. } => {
            ("triggered", description.as_deref())
        }
        StackEntryKind::KeywordAction { .. } => ("keyword", None),
    };
    let targets = entry
        .ability()
        .map(flatten_targets_in_chain)
        .unwrap_or_default();
    let paid = state.stack_paid_facts.get(&entry.id).cloned();
    let trigger_context = state
        .stack_trigger_event_batches
        .get(&entry.id)
        .map(|events| events.iter().map(|event| format!("{event:?}")).collect())
        .or_else(|| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                trigger_event: Some(event),
                ..
            } => Some(vec![format!("{event:?}")]),
            _ => None,
        })
        .unwrap_or_default();
    StackGroupKey {
        source_name,
        tag,
        description: description.map(str::to_owned),
        targets,
        paid,
        trigger_context,
    }
}

/// CR 110.4b: A permanent spell — "an artifact, battle, creature, enchantment,
/// or planeswalker spell." Lands are excluded because they aren't spells
/// (they're played, not cast). Used by resolution paths that distinguish
/// "spell that will enter the battlefield" from "non-permanent spell"
/// (e.g., Sneak's CR 702.190b alongside-attacker placement, which applies
/// only to permanent spells).
pub(crate) fn is_permanent_spell(state: &GameState, object_id: ObjectId) -> bool {
    use crate::types::card_type::CoreType;

    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    obj.card_types.core_types.iter().any(|ct| {
        matches!(
            ct,
            CoreType::Artifact
                | CoreType::Battle
                | CoreType::Creature
                | CoreType::Enchantment
                | CoreType::Planeswalker
        )
    })
}

/// CR 702.185a: Create the Warp delayed trigger that exiles the permanent at end step
/// and grants WarpExile casting permission. Shared between resolve_top (Execute path)
/// and engine_replacement (NeedsChoice path).
pub(crate) fn create_warp_delayed_trigger(
    state: &mut GameState,
    object_id: ObjectId,
    controller: crate::types::player::PlayerId,
) {
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, CastingPermission, DelayedTriggerCondition, Effect,
        ResolvedAbility,
    };
    use crate::types::phase::Phase;

    let exile_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: Some(Zone::Battlefield),
            destination: Zone::Exile,
            target: crate::types::ability::TargetFilter::SelfRef,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GrantCastingPermission {
            permission: CastingPermission::WarpExile {
                castable_after_turn: state.turn_number,
            },
            target: crate::types::ability::TargetFilter::SelfRef,
            grantee: crate::types::ability::PermissionGrantee::AbilityController,
        },
    ));

    let mut delayed_ability =
        ResolvedAbility::new(*exile_def.effect, vec![], object_id, controller);
    if let Some(sub) = exile_def.sub_ability {
        delayed_ability = delayed_ability.sub_ability(ResolvedAbility::new(
            *sub.effect,
            vec![],
            object_id,
            controller,
        ));
    }

    state
        .delayed_triggers
        .push(crate::types::game_state::DelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            ability: delayed_ability,
            controller,
            source_id: object_id,
            one_shot: true,
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        CostPaidObjectSnapshot, Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
        TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    fn create_aura_on_stack(state: &mut GameState, target_id: ObjectId) -> ObjectId {
        let aura_id = create_object(
            state,
            CardId(100),
            PlayerId(0),
            "Pacifism".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&aura_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.keywords.push(Keyword::Enchant(
                crate::types::ability::TargetFilter::Typed(TypedFilter::creature()),
            ));
        }

        let resolved = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "Aura".to_string(),
                description: None,
            },
            vec![TargetRef::Object(target_id)],
            aura_id,
            PlayerId(0),
        );

        state.stack.push_back(StackEntry {
            id: aura_id,
            source_id: aura_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(100),
                ability: Some(resolved),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        aura_id
    }

    #[test]
    fn permanent_spell_resolution_links_exiled_cost_paid_object() {
        let mut state = setup();
        let exiled_id = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Exiled Elemental".to_string(),
            Zone::Exile,
        );
        let snapshot = {
            let exiled = state.objects.get(&exiled_id).unwrap();
            CostPaidObjectSnapshot {
                object_id: exiled_id,
                lki: exiled.snapshot_for_mana_spent(),
            }
        };
        let spell_id = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Champion of the Path".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "behold-cost-regression".to_string(),
                description: None,
            },
            vec![],
            spell_id,
            PlayerId(0),
        );
        ability.set_cost_paid_object_recursive(snapshot);

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(102),
                ability: Some(ability),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert!(state.battlefield.contains(&spell_id));
        assert!(state.exile_links.iter().any(|link| {
            link.exiled_id == exiled_id
                && link.source_id == spell_id
                && matches!(
                    link.kind,
                    ExileLinkKind::UntilSourceLeaves {
                        return_zone: Zone::Hand
                    }
                )
        }));
    }

    /// CR 110.4b + CR 608.3 + CR 310.4b: Battle spells are permanent spells.
    /// They resolve to the battlefield, not to their owner's graveyard, and
    /// receive their intrinsic defense counters through the ETB replacement
    /// pipeline.
    #[test]
    fn battle_spell_resolves_to_battlefield_with_defense_counters() {
        let mut state = setup();
        let battle_id = create_object(
            &mut state,
            CardId(622),
            PlayerId(0),
            "Test Siege".to_string(),
            Zone::Stack,
        );
        {
            let battle = state.objects.get_mut(&battle_id).unwrap();
            battle.card_types.core_types.push(CoreType::Battle);
            battle.card_types.subtypes.push("Siege".to_string());
            battle.defense = Some(4);
            battle.base_defense = Some(4);
        }

        state.stack.push_back(StackEntry {
            id: battle_id,
            source_id: battle_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(622),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(state.objects[&battle_id].zone, Zone::Battlefield);
        assert!(state.battlefield.contains(&battle_id));
        assert!(!state.players[0].graveyard.contains(&battle_id));
        assert_eq!(
            state.objects[&battle_id]
                .counters
                .get(&CounterType::Defense)
                .copied(),
            Some(4)
        );
    }

    #[test]
    fn trigger_event_context_becomes_target_controller() {
        // Set up: triggered ability with BecomesTarget event in trigger_event.
        // Verify: at resolution, current_trigger_event is set so
        // TriggeringSpellController can resolve to the controller of the source.
        let mut state = setup();

        // Create a "spell" object controlled by player 1 that is the source in BecomesTarget
        let spell_id = create_object(
            &mut state,
            CardId(80),
            PlayerId(1),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );

        let trigger_event = GameEvent::BecomesTarget {
            target: TargetRef::Object(ObjectId(999)), // target doesn't matter for this test
            source_id: spell_id,
        };

        // Build a triggered ability that would want to resolve TriggeringSpellController
        let resolved = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "EventContextTest".to_string(),
                description: None,
            },
            vec![],
            ObjectId(50),
            PlayerId(0),
        );

        let entry_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;

        state.stack.push_back(StackEntry {
            id: entry_id,
            source_id: ObjectId(50),
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: ObjectId(50),
                ability: Box::new(resolved),
                condition: None,
                trigger_event: Some(trigger_event.clone()),
                description: None,
                source_name: String::new(),
                subject_match_count: None,
            },
        });

        // Before resolution, current_trigger_event should be None
        assert!(state.current_trigger_event.is_none());

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // After resolution, current_trigger_event should be cleared
        assert!(state.current_trigger_event.is_none());

        // Verify the event was set during resolution by checking the resolve happened
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::StackResolved { .. })));

        // Verify event-context resolution works with the trigger event
        // by manually setting and checking the resolution function
        state.current_trigger_event = Some(trigger_event);
        let result = crate::game::targeting::resolve_event_context_target(
            &state,
            &crate::types::ability::TargetFilter::TriggeringSpellController,
            ObjectId(50),
        );
        assert_eq!(result, Some(TargetRef::Player(PlayerId(1))));

        // TriggeringSpellOwner should return the owner
        let result = crate::game::targeting::resolve_event_context_target(
            &state,
            &crate::types::ability::TargetFilter::TriggeringSpellOwner,
            ObjectId(50),
        );
        assert_eq!(result, Some(TargetRef::Player(PlayerId(1))));

        // TriggeringSource should return the source object
        let result = crate::game::targeting::resolve_event_context_target(
            &state,
            &crate::types::ability::TargetFilter::TriggeringSource,
            ObjectId(50),
        );
        assert_eq!(result, Some(TargetRef::Object(spell_id)));

        // Clean up
        state.current_trigger_event = None;
    }

    #[test]
    fn trigger_event_context_no_event_returns_none() {
        let state = setup();
        // With no current_trigger_event, resolution should return None
        let result = crate::game::targeting::resolve_event_context_target(
            &state,
            &crate::types::ability::TargetFilter::TriggeringSpellController,
            ObjectId(1),
        );
        assert!(result.is_none());
    }

    #[test]
    fn aura_resolving_attaches_to_target() {
        let mut state = setup();

        // Create a creature on the battlefield
        let creature = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Create an Aura spell targeting the creature
        let aura_id = create_aura_on_stack(&mut state, creature);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Aura should be on the battlefield
        assert!(state.battlefield.contains(&aura_id));
        // Aura should be attached to the creature
        assert_eq!(
            state
                .objects
                .get(&aura_id)
                .unwrap()
                .attached_to
                .and_then(|t| t.as_object()),
            Some(creature)
        );
        // Creature should list the Aura in its attachments
        assert!(state
            .objects
            .get(&creature)
            .unwrap()
            .attachments
            .contains(&aura_id));
    }

    #[test]
    fn aura_fizzles_when_target_left_battlefield() {
        let mut state = setup();

        // Create a creature, then remove it from battlefield before resolution
        let creature = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let aura_id = create_aura_on_stack(&mut state, creature);

        // Remove creature from battlefield before resolution
        state.battlefield.retain(|&id| id != creature);
        if let Some(obj) = state.objects.get_mut(&creature) {
            obj.zone = Zone::Graveyard;
        }
        state.players[1].graveyard.push_back(creature);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Aura should fizzle to graveyard (not to battlefield)
        assert!(!state.battlefield.contains(&aura_id));
        assert!(state.players[0].graveyard.contains(&aura_id));
    }

    #[test]
    fn non_aura_permanent_resolving_no_attachment() {
        let mut state = setup();

        // Create a non-Aura enchantment on the stack
        let ench_id = create_object(
            &mut state,
            CardId(60),
            PlayerId(0),
            "Intangible Virtue".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&ench_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);

        state.stack.push_back(StackEntry {
            id: ench_id,
            source_id: ench_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(60),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Should be on battlefield, not attached to anything
        assert!(state.battlefield.contains(&ench_id));
        assert_eq!(state.objects.get(&ench_id).unwrap().attached_to, None);
    }

    #[test]
    fn multi_target_chain_resolves_remaining_legal_target() {
        let mut state = setup();

        let first_target = create_object(
            &mut state,
            CardId(70),
            PlayerId(1),
            "First Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&first_target).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(3);
            obj.toughness = Some(3);
        }

        let second_target = create_object(
            &mut state,
            CardId(71),
            PlayerId(1),
            "Second Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&second_target).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(3);
            obj.toughness = Some(3);
        }

        let spell_id = create_object(
            &mut state,
            CardId(72),
            PlayerId(0),
            "Twin Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: crate::types::ability::TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
            },
            vec![TargetRef::Object(first_target)],
            spell_id,
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: crate::types::ability::TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
            },
            vec![TargetRef::Object(second_target)],
            spell_id,
            PlayerId(0),
        ));

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(72),
                ability: Some(ability),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        state.battlefield.retain(|&id| id != first_target);
        state.objects.get_mut(&first_target).unwrap().zone = Zone::Graveyard;
        state.players[1].graveyard.push_back(first_target);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert!(state.players[0].graveyard.contains(&spell_id));
        assert_eq!(state.objects[&second_target].damage_marked, 2);
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::DamageDealt {
                    target: TargetRef::Object(target),
                    amount: 2,
                    ..
                } if *target == second_target
            )),
            "expected the remaining legal target to be damaged"
        );
    }

    #[test]
    fn warp_delayed_trigger_grants_warp_exile_not_alt_cost() {
        // CR 702.185a: The delayed trigger should grant WarpExile (normal cost),
        // not ExileWithAltCost (which would use the warp cost).
        use crate::types::ability::CastingPermission;
        use crate::types::game_state::{StackEntry, StackEntryKind};
        use crate::types::mana::ManaCost;

        let mut state = setup();
        state.turn_number = 3;
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Warp Creature".to_string(),
            Zone::Battlefield,
        );
        // Give the object a Warp keyword with a cheap cost {R}
        // and a different normal cost {2}{R}
        let warp_cost = ManaCost::Cost {
            shards: vec![crate::types::mana::ManaCostShard::Red],
            generic: 0,
        };
        let normal_cost = ManaCost::Cost {
            shards: vec![crate::types::mana::ManaCostShard::Red],
            generic: 2,
        };
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.keywords.push(Keyword::Warp(warp_cost));
            obj.mana_cost = normal_cost;
            obj.card_types.core_types.push(CoreType::Creature);
        }

        // Push a stack entry as if cast via Warp
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: crate::types::game_state::CastingVariant::Warp,
                actual_mana_spent: 0,
            },
        });

        // Resolve the stack entry — this should create a Warp delayed trigger
        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Verify a delayed trigger was created
        assert_eq!(
            state.delayed_triggers.len(),
            1,
            "should have created one delayed trigger"
        );

        // Check the delayed trigger's sub_ability grants WarpExile
        let trigger = &state.delayed_triggers[0];
        let sub = trigger
            .ability
            .sub_ability
            .as_ref()
            .expect("should have sub_ability");
        match &sub.effect {
            Effect::GrantCastingPermission { permission, .. } => match permission {
                CastingPermission::WarpExile {
                    castable_after_turn,
                } => {
                    assert_eq!(
                        *castable_after_turn, 3,
                        "castable_after_turn should match the turn number at resolution"
                    );
                }
                other => panic!("expected WarpExile, got {other:?}"),
            },
            other => panic!("expected GrantCastingPermission, got {other:?}"),
        }
    }

    #[test]
    fn warp_exile_respects_turn_restriction() {
        // CR 702.185a: WarpExile cards should not be castable on the same turn
        // they were exiled, only after the turn ends.
        use crate::game::casting::spell_objects_available_to_cast;
        use crate::types::ability::CastingPermission;

        let mut state = setup();
        state.turn_number = 3;

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Warp Creature".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.casting_permissions.push(CastingPermission::WarpExile {
                castable_after_turn: 3,
            });
        }

        // On the same turn (turn 3): should NOT be castable
        let available = spell_objects_available_to_cast(&state, PlayerId(0));
        assert!(
            !available.contains(&obj_id),
            "WarpExile card should NOT be castable on the same turn it was exiled"
        );

        // On the next turn (turn 4): should be castable
        state.turn_number = 4;
        let available = spell_objects_available_to_cast(&state, PlayerId(0));
        assert!(
            available.contains(&obj_id),
            "WarpExile card should be castable after the exile turn ends"
        );
    }

    #[test]
    fn warp_exile_does_not_emit_airbend_event() {
        // CR 702.185a: WarpExile permissions should NOT trigger Airbend events.
        use crate::types::ability::{CastingPermission, Effect, TargetFilter};

        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Warp Card".to_string(),
            Zone::Exile,
        );

        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::WarpExile {
                    castable_after_turn: 1,
                },
                target: TargetFilter::SelfRef,
                grantee: crate::types::ability::PermissionGrantee::AbilityController,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        crate::game::effects::grant_permission::resolve(&mut state, &ability, &mut events).unwrap();

        // Verify permission was granted
        let obj = state.objects.get(&obj_id).unwrap();
        assert!(
            obj.casting_permissions
                .iter()
                .any(|p| matches!(p, CastingPermission::WarpExile { .. })),
            "WarpExile permission should be on the object"
        );

        // Verify no Airbend event was emitted
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, crate::types::events::GameEvent::Airbend { .. })),
            "WarpExile should NOT emit Airbend event"
        );
    }

    #[test]
    fn exile_with_alt_cost_still_works() {
        // Regression: ExileWithAltCost (Airbending, etc.) should still be immediately castable.
        use crate::game::casting::spell_objects_available_to_cast;
        use crate::types::ability::CastingPermission;
        use crate::types::mana::ManaCost;

        let mut state = setup();
        state.turn_number = 5;

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Airbent Card".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: ManaCost::generic(2),
                    cast_transformed: false,
                    constraint: None,
                    granted_to: None,
                });
        }

        // Should be immediately castable (no turn restriction)
        let available = spell_objects_available_to_cast(&state, PlayerId(0));
        assert!(
            available.contains(&obj_id),
            "ExileWithAltCost should be immediately castable (no turn restriction)"
        );
    }

    // -----------------------------------------------------------------------
    // Flashback zone routing (CR 702.34a)
    // -----------------------------------------------------------------------

    /// Helper: push a Flashback spell onto the stack and return its ObjectId.
    fn push_flashback_spell(state: &mut GameState, effect: Effect) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let obj_id = create_object(
            state,
            card_id,
            PlayerId(0),
            "Flashback Spell".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
        }
        let resolved = ResolvedAbility::new(effect, vec![], obj_id, PlayerId(0));
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id,
                ability: Some(resolved),
                casting_variant: CastingVariant::Flashback,
                actual_mana_spent: 0,
            },
        });
        obj_id
    }

    fn push_graveyard_permission_spell_with_exile_rider(
        state: &mut GameState,
        effect: Effect,
    ) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let obj_id = create_object(
            state,
            card_id,
            PlayerId(0),
            "Permission Spell".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
        }
        let resolved = ResolvedAbility::new(effect, vec![], obj_id, PlayerId(0));
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id,
                ability: Some(resolved),
                casting_variant: CastingVariant::GraveyardPermission {
                    source: ObjectId(999),
                    frequency: crate::types::statics::CastFrequency::OncePerTurn,
                    slot_type: None,
                    graveyard_destination_replacement: Some(Zone::Exile),
                },
                actual_mana_spent: 0,
            },
        });
        obj_id
    }

    #[test]
    fn flashback_spell_exiles_on_resolution() {
        let mut state = setup();
        let obj_id = push_flashback_spell(
            &mut state,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&obj_id].zone,
            Zone::Exile,
            "Flashback spell should be exiled on resolution, not sent to graveyard"
        );
    }

    #[test]
    fn graveyard_permission_exile_rider_exiles_on_resolution() {
        let mut state = setup();
        let obj_id = push_graveyard_permission_spell_with_exile_rider(
            &mut state,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&obj_id].zone,
            Zone::Exile,
            "graveyard permission rider should replace the normal resolution graveyard destination"
        );
    }

    #[test]
    fn flashback_spell_exiles_on_fizzle() {
        let mut state = setup();

        // Create a target creature that we'll remove to cause fizzle
        let target_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(1),
            "Target Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }
        state.battlefield.push_back(target_id);

        // Push a flashback spell targeting that creature
        let card_id = CardId(state.next_object_id);
        let spell_id = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Flashback Bolt".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
        }
        let resolved = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Object(target_id)],
            spell_id,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id,
                ability: Some(resolved),
                casting_variant: CastingVariant::Flashback,
                actual_mana_spent: 0,
            },
        });

        // Remove the target to cause fizzle
        zones::move_to_zone(&mut state, target_id, Zone::Graveyard, &mut Vec::new());

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&spell_id].zone,
            Zone::Exile,
            "Flashback spell should be exiled on fizzle, not sent to graveyard"
        );
    }

    #[test]
    fn stack_pressure_boundaries() {
        let mut state = GameState::new_two_player(42);
        assert_eq!(stack_pressure(&state), StackPressure::Normal);

        // Synthesize entries; kind/source doesn't matter for pressure.
        fn push_n(state: &mut GameState, n: usize) {
            use crate::types::card_type::CoreType;
            use crate::types::identifiers::{CardId, ObjectId};
            let src = crate::game::zones::create_object(
                state,
                CardId(1),
                PlayerId(0),
                "filler".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&src)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
            for i in 0..n {
                state.stack.push_back(StackEntry {
                    id: ObjectId(100_000 + i as u64),
                    source_id: src,
                    controller: PlayerId(0),
                    kind: StackEntryKind::Spell {
                        card_id: CardId(1),
                        ability: None,
                        casting_variant: CastingVariant::default(),
                        actual_mana_spent: 0,
                    },
                });
            }
        }

        // 9 entries → still Normal
        push_n(&mut state, 9);
        assert_eq!(stack_pressure(&state), StackPressure::Normal);
        // 10th crosses Elevated
        push_n(&mut state, 1);
        assert_eq!(stack_pressure(&state), StackPressure::Elevated);
        // 29 total → still Elevated
        push_n(&mut state, 19);
        assert_eq!(stack_pressure(&state), StackPressure::Elevated);
        // 30th crosses Rapid
        push_n(&mut state, 1);
        assert_eq!(stack_pressure(&state), StackPressure::Rapid);
        // 99 total → still Rapid
        push_n(&mut state, 69);
        assert_eq!(stack_pressure(&state), StackPressure::Rapid);
        // 100th crosses Instant
        push_n(&mut state, 1);
        assert_eq!(stack_pressure(&state), StackPressure::Instant);
    }

    #[test]
    fn stack_display_groups_coalesce_identical_triggers() {
        use crate::types::ability::{Effect, ResolvedAbility};
        use crate::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        let mk_effect = || Effect::Unimplemented {
            name: "test".to_string(),
            description: None,
        };

        // 100 Scute-Swarm-like sources all sharing the same name — each fires
        // its own copy of the ETB trigger. The group key (source name + kind
        // + description) collapses them.
        for i in 0..100 {
            let sid = crate::game::zones::create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Scute Swarm".to_string(),
                Zone::Battlefield,
            );
            state.stack.push_back(StackEntry {
                id: ObjectId(10_000 + i as u64),
                source_id: sid,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: sid,
                    ability: Box::new(ResolvedAbility::new(mk_effect(), vec![], sid, PlayerId(0))),
                    condition: None,
                    trigger_event: None,
                    description: Some("landfall copy trigger".to_string()),
                    source_name: String::new(),
                    subject_match_count: None,
                },
            });
        }

        let groups = stack_display_groups(&state);
        assert_eq!(
            groups.len(),
            1,
            "100 identical Scute Swarm triggers should collapse to one group"
        );
        assert_eq!(groups[0].count, 100);
        assert_eq!(groups[0].member_ids.len(), 100);
    }

    #[test]
    fn stack_display_groups_distinguish_different_sources() {
        use crate::types::ability::{Effect, ResolvedAbility};
        use crate::types::identifiers::CardId;

        let mut state = GameState::new_two_player(42);
        let s1 = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Scute Swarm".to_string(),
            Zone::Battlefield,
        );
        let s2 = crate::game::zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Impact Tremors".to_string(),
            Zone::Battlefield,
        );
        let mk_effect = || Effect::Unimplemented {
            name: "test".to_string(),
            description: None,
        };
        let mk_entry = |sid| StackEntry {
            id: sid,
            source_id: sid,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: sid,
                ability: Box::new(ResolvedAbility::new(mk_effect(), vec![], sid, PlayerId(0))),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
            },
        };
        state.stack.push_back(mk_entry(s1));
        state.stack.push_back(mk_entry(s2));

        let groups = stack_display_groups(&state);
        assert_eq!(
            groups.len(),
            2,
            "different-named sources must stay separate"
        );
        assert_eq!(groups[0].count, 1);
        assert_eq!(groups[1].count, 1);
    }

    /// Two visually-identical triggers that target different players must NOT
    /// coalesce — coalescing them would misrepresent the resolved targeting.
    /// Regression guard for the target-signature component of `group_key`.
    #[test]
    fn stack_display_groups_distinguish_different_targets() {
        use crate::types::ability::{Effect, ResolvedAbility, TargetRef};
        use crate::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        let sid = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Syphon Life".to_string(),
            Zone::Battlefield,
        );
        let mk_effect = || Effect::Unimplemented {
            name: "test".to_string(),
            description: None,
        };
        let mk_entry = |id: u64, target: TargetRef| StackEntry {
            id: ObjectId(id),
            source_id: sid,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: sid,
                ability: Box::new(ResolvedAbility::new(
                    mk_effect(),
                    vec![target],
                    sid,
                    PlayerId(0),
                )),
                condition: None,
                trigger_event: None,
                description: Some("target player loses 1 life".to_string()),
                source_name: String::new(),
                subject_match_count: None,
            },
        };
        state
            .stack
            .push_back(mk_entry(10_001, TargetRef::Player(PlayerId(0))));
        state
            .stack
            .push_back(mk_entry(10_002, TargetRef::Player(PlayerId(1))));

        let groups = stack_display_groups(&state);
        assert_eq!(
            groups.len(),
            2,
            "triggers with divergent targets must not coalesce: got {:?}",
            groups
        );
    }

    #[test]
    fn stack_display_groups_distinguish_chained_targets() {
        use crate::types::ability::{Effect, ResolvedAbility, TargetRef};
        use crate::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        let sid = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Chained Trigger".to_string(),
            Zone::Battlefield,
        );
        let mk_effect = || Effect::Unimplemented {
            name: "test".to_string(),
            description: None,
        };
        let mk_entry = |id: u64, target: TargetRef| {
            let mut ability = ResolvedAbility::new(mk_effect(), Vec::new(), sid, PlayerId(0));
            ability.sub_ability = Some(Box::new(ResolvedAbility::new(
                mk_effect(),
                vec![target],
                sid,
                PlayerId(0),
            )));
            StackEntry {
                id: ObjectId(id),
                source_id: sid,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: sid,
                    ability: Box::new(ability),
                    condition: None,
                    trigger_event: None,
                    description: Some("then target player loses 1 life".to_string()),
                    source_name: String::new(),
                    subject_match_count: None,
                },
            }
        };
        state
            .stack
            .push_back(mk_entry(10_001, TargetRef::Player(PlayerId(0))));
        state
            .stack
            .push_back(mk_entry(10_002, TargetRef::Player(PlayerId(1))));

        let groups = stack_display_groups(&state);
        assert_eq!(
            groups.len(),
            2,
            "chained targets must participate in stack grouping; got {:?}",
            groups
        );
    }

    /// KeywordAction entries (Equip/Crew/etc.) carry their targets inside
    /// the enum variant, invisible to the target-aware `group_key`. To
    /// avoid an M1-style target-coalescing bug, `stack_display_groups`
    /// opts keyword-action entries out of coalescing entirely — each gets
    /// its own group regardless of source/target identity. Regression
    /// guard for that behavior.
    #[test]
    fn stack_display_groups_never_coalesce_keyword_actions() {
        use crate::types::ability::KeywordAction;
        use crate::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        let equip = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bonesplitter".to_string(),
            Zone::Battlefield,
        );
        let creature_a = crate::game::zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Grizzly Bears A".to_string(),
            Zone::Battlefield,
        );
        let creature_b = crate::game::zones::create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Grizzly Bears B".to_string(),
            Zone::Battlefield,
        );
        let mk_entry = |id: u64, target: ObjectId| StackEntry {
            id: ObjectId(id),
            source_id: equip,
            controller: PlayerId(0),
            kind: StackEntryKind::KeywordAction {
                action: KeywordAction::Equip {
                    equipment_id: equip,
                    target_creature_id: target,
                },
            },
        };
        state.stack.push_back(mk_entry(10_001, creature_a));
        state.stack.push_back(mk_entry(10_002, creature_b));

        let groups = stack_display_groups(&state);
        assert_eq!(
            groups.len(),
            2,
            "two Equip activations on different targets must not coalesce; got {:?}",
            groups
        );
    }

    /// CR 702.27a: Build an instant spell on the stack with a draw effect and
    /// a `Keyword::Buyback` on the game object. `buyback_paid` controls
    /// `ability.context.additional_cost_paid`. Returns the spell's object id.
    fn push_buyback_spell(state: &mut GameState, buyback_paid: bool) -> ObjectId {
        use crate::types::keywords::{BuybackCost, Keyword};
        use crate::types::mana::ManaCost;
        let spell_id = create_object(
            state,
            CardId(300),
            PlayerId(0),
            "Whispers of the Muse".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.keywords
                .push(Keyword::Buyback(BuybackCost::Mana(ManaCost::Cost {
                    generic: 5,
                    shards: vec![],
                })));
        }

        let mut resolved = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            spell_id,
            PlayerId(0),
        );
        resolved.context.additional_cost_paid = buyback_paid;

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(300),
                ability: Some(resolved),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        spell_id
    }

    /// CR 702.27a: When the buyback cost was paid, the spell returns to its
    /// owner's hand instead of the graveyard as it resolves.
    #[test]
    fn buyback_paid_routes_resolving_spell_to_hand() {
        let mut state = setup();
        let spell_id = push_buyback_spell(&mut state, true);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert!(
            state.players[0].hand.contains(&spell_id),
            "buyback-paid spell should return to owner's hand"
        );
        assert!(
            !state.players[0].graveyard.contains(&spell_id),
            "buyback-paid spell must not go to graveyard"
        );
    }

    /// CR 608.2n: Without the buyback cost paid, the non-permanent spell
    /// goes to its owner's graveyard normally.
    #[test]
    fn buyback_not_paid_routes_resolving_spell_to_graveyard() {
        let mut state = setup();
        let spell_id = push_buyback_spell(&mut state, false);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert!(
            state.players[0].graveyard.contains(&spell_id),
            "non-buyback spell should go to owner's graveyard"
        );
        assert!(
            !state.players[0].hand.contains(&spell_id),
            "non-buyback spell must not return to hand"
        );
    }

    /// Helper: build a permanent (creature) spell whose on-resolve ability
    /// self-exiles via `ChangeZone { target: SelfRef, destination: Exile }`.
    /// Pushes it onto the stack and returns its object id.
    ///
    /// This shape doesn't appear in the printed corpus today, but the post-#323
    /// architectural contract is: any spell whose own resolution moves it off
    /// the Stack must NOT also receive the post-resolution default zone move
    /// (Stack→Battlefield for permanents, Stack→Graveyard for non-permanents,
    /// Stack→Graveyard on prevented ETB). The Stack-residency guard
    /// (`spell_still_on_stack`) is the single authority for this gate.
    fn push_self_exiling_permanent_spell(state: &mut GameState) -> ObjectId {
        let spell_id = create_object(
            state,
            CardId(900),
            PlayerId(0),
            "Test Self-Exiling Creature".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
            obj.power = Some(1);
            obj.toughness = Some(1);
        }

        let resolved = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![],
            spell_id,
            PlayerId(0),
        );

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(900),
                ability: Some(resolved),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        spell_id
    }

    fn push_self_exiling_aura_spell(state: &mut GameState, target_id: ObjectId) -> ObjectId {
        let spell_id = create_object(
            state,
            CardId(901),
            PlayerId(0),
            "Test Self-Exiling Aura".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
        }

        let resolved = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![TargetRef::Object(target_id)],
            spell_id,
            PlayerId(0),
        );

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(901),
                ability: Some(resolved),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        spell_id
    }

    /// CR 608.3 + CR 608.2c (architectural cleanup, deferred from #323): a
    /// permanent spell whose `execute_effect` self-exiles must NOT be moved to
    /// the battlefield by the post-resolution Stack→Battlefield default. The
    /// Stack-residency guard (`spell_still_on_stack`) is the single authority
    /// — the same predicate already guards the non-permanent CR 608.2n
    /// graveyard default.
    ///
    /// Pre-fix the permanent-resolution branch in `resolve_top` would call
    /// `move_to_zone(state, object_id, to, events)` unconditionally, undoing
    /// the spell's own self-exile clause and corrupting the object's zone
    /// state by treating the exiled card as if it had entered the battlefield
    /// (ETB-tapped, ETB-counters, transform).
    #[test]
    fn permanent_spell_self_exile_skips_battlefield_default() {
        let mut state = setup();
        let spell_id = push_self_exiling_permanent_spell(&mut state);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&spell_id].zone,
            Zone::Exile,
            "permanent spell with self-exile sub-ability must end in Exile, \
             not Battlefield (post-resolution default must be skipped when the \
             spell already left the Stack during execute_effect)"
        );
        assert!(
            !state.battlefield.contains(&spell_id),
            "self-exiled permanent must NOT be added to the battlefield zone index"
        );
        assert!(
            state.exile.contains(&spell_id),
            "self-exiled permanent must be tracked in the exile zone index"
        );
    }

    #[test]
    fn self_moved_aura_spell_does_not_receive_battlefield_attachment_side_effects() {
        let mut state = setup();
        let target_id = create_object(
            &mut state,
            CardId(902),
            PlayerId(0),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }
        let aura_id = push_self_exiling_aura_spell(&mut state, target_id);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(state.objects[&aura_id].zone, Zone::Exile);
        assert!(
            state.objects[&aura_id].attached_to.is_none(),
            "Aura post-resolution attachment must only run after actual battlefield entry"
        );
        assert!(
            !state.objects[&target_id].attachments.contains(&aura_id),
            "target must not point at an Aura that self-exiled during resolution"
        );
    }

    /// CR 608.3e: a permanent spell whose ETB is fully prevented goes to its
    /// owner's graveyard only if it is still on the stack when that fallback is
    /// reached.
    #[test]
    fn prevented_etb_default_only_moves_spell_still_on_stack() {
        let mut state = setup();
        let spell_id = create_object(
            &mut state,
            CardId(903),
            PlayerId(0),
            "Test Prevented Creature".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let mut events = Vec::new();

        move_prevented_permanent_spell_to_graveyard_if_still_on_stack(
            &mut state,
            spell_id,
            &mut events,
        );
        assert_eq!(state.objects[&spell_id].zone, Zone::Graveyard);

        zones::move_to_zone(&mut state, spell_id, Zone::Exile, &mut events);
        move_prevented_permanent_spell_to_graveyard_if_still_on_stack(
            &mut state,
            spell_id,
            &mut events,
        );
        assert_eq!(state.objects[&spell_id].zone, Zone::Exile);
    }

    // ── Tier 3: batch-resolution tests ───────────────────────────────────
    //
    // These drive the REAL resolution pipeline (resolve_next / resolve_top +
    // run_post_action_pipeline) — they are runtime tests, not shape tests.

    mod batch_resolve {
        // Driver internals under test (the stack module).
        use super::super::{
            batch_run_len, effects, observers_are_batch_safe, resolve_next, resolve_top,
        };
        // Test fixtures from the parent `tests` module.
        use super::setup;
        use crate::game::triggers;
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCondition, AbilityDefinition, Comparator, Duration, Effect, PtValue,
            QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter, TargetRef, TriggerCondition,
            TriggerDefinition, TypeFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::counter::CounterType;
        use crate::types::game_state::{GameState, StackEntry, StackEntryKind};
        use crate::types::identifiers::{CardId, ObjectId};
        use crate::types::mana::ManaColor;
        use crate::types::player::PlayerId;
        use crate::types::proposed_event::TokenSpec;
        use crate::types::triggers::TriggerMode;
        use crate::types::zones::Zone;
        use std::sync::Arc;

        /// A bare Insect Token effect: 1/1 green Insect, Fixed count.
        fn insect_token_effect() -> Effect {
            Effect::Token {
                name: "Insect".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string()],
                colors: vec![ManaColor::Green],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            }
        }

        /// Put `n` Forests on the battlefield under player 0.
        fn add_lands(state: &mut GameState, n: usize) {
            for _ in 0..n {
                let id = create_object(
                    state,
                    CardId(900),
                    PlayerId(0),
                    "Forest".to_string(),
                    Zone::Battlefield,
                );
                state
                    .objects
                    .get_mut(&id)
                    .unwrap()
                    .card_types
                    .core_types
                    .push(CoreType::Land);
            }
        }

        /// Create a Scute-Swarm-style source permanent (landfall trigger) on the
        /// battlefield and return its id. The landfall trigger registers under
        /// `EnterBattlefield(Some(Land))`, so (mirroring real Scute Swarm) it
        /// never matches the creature-token probe — the source's own trigger does
        /// not block batching, while a creature-keyed observer (CR 603.3) does.
        fn add_scute_source(state: &mut GameState) -> ObjectId {
            let id = create_object(
                state,
                CardId(901),
                PlayerId(0),
                "Scute Swarm".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&id).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                let landfall = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Land],
                        ..Default::default()
                    }));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(landfall.clone());
                obj.trigger_definitions.push(landfall);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
            id
        }

        /// Push `n` identical untargeted Token triggers from `source_id`.
        fn push_token_triggers(
            state: &mut GameState,
            source_id: ObjectId,
            effect: Effect,
            sub_ability: Option<Box<ResolvedAbility>>,
            n: usize,
        ) {
            for _ in 0..n {
                let entry_id = ObjectId(state.next_object_id);
                state.next_object_id += 1;
                let mut ability =
                    ResolvedAbility::new(effect.clone(), vec![], source_id, PlayerId(0));
                ability.sub_ability = sub_ability.clone();
                state.stack.push_back(StackEntry {
                    id: entry_id,
                    source_id,
                    controller: PlayerId(0),
                    kind: StackEntryKind::TriggeredAbility {
                        source_id,
                        ability: Box::new(ability),
                        condition: None,
                        trigger_event: None,
                        description: Some("Landfall".to_string()),
                        source_name: "Scute Swarm".to_string(),
                        subject_match_count: None,
                    },
                });
            }
        }

        /// Drive resolution to empty via the BATCH path (`resolve_next`), running
        /// the real post-action pipeline after each step. Returns the per-step
        /// `consumed` counts.
        fn resolve_to_empty_batched(state: &mut GameState) -> Vec<u32> {
            let mut steps = Vec::new();
            let mut guard = 0;
            while !state.stack.is_empty() {
                let mut events = Vec::new();
                let consumed = resolve_next(state, &mut events);
                steps.push(consumed);
                triggers::process_triggers(state, &events);
                crate::game::sba::check_state_based_actions(state, &mut events);
                guard += 1;
                assert!(guard < 10_000, "resolution did not terminate");
            }
            steps
        }

        /// Drive resolution to empty via the SEQUENTIAL path (`resolve_top`),
        /// running the real post-action pipeline after each step.
        fn resolve_to_empty_sequential(state: &mut GameState) {
            let mut guard = 0;
            while !state.stack.is_empty() {
                let mut events = Vec::new();
                resolve_top(state, &mut events);
                triggers::process_triggers(state, &events);
                crate::game::sba::check_state_based_actions(state, &mut events);
                guard += 1;
                assert!(guard < 10_000, "resolution did not terminate");
            }
        }

        fn token_ids(state: &GameState) -> Vec<ObjectId> {
            state
                .battlefield
                .iter()
                .copied()
                .filter(|id| state.objects.get(id).is_some_and(|o| o.is_token))
                .collect()
        }

        // §9.2 — observer-free positive batch case (sub-6 lands base Insect).
        #[test]
        fn observer_free_batch_equals_sequential() {
            let mut base = setup();
            add_lands(&mut base, 3);
            let src = add_scute_source(&mut base);
            push_token_triggers(&mut base, src, insect_token_effect(), None, 10);

            let mut batched = base.clone();
            let mut sequential = base.clone();

            let steps = resolve_to_empty_batched(&mut batched);
            resolve_to_empty_sequential(&mut sequential);

            // The 10 entries collapsed into a single batched step.
            assert_eq!(steps, vec![10], "expected one 10-entry batch");
            // Exactly 10 Insect tokens, identical to the sequential path.
            assert_eq!(token_ids(&batched).len(), 10);
            assert_eq!(token_ids(&sequential).len(), 10);
            assert_eq!(batched.battlefield.len(), sequential.battlefield.len());
            assert!(batched.stack.is_empty() && sequential.stack.is_empty());
            for id in token_ids(&batched) {
                let o = &batched.objects[&id];
                assert_eq!(o.power, Some(1));
                assert_eq!(o.toughness, Some(1));
                assert!(o.card_types.core_types.contains(&CoreType::Creature));
            }
        }

        // §9.2 — Layer C reports safe on an observer-free board.
        #[test]
        fn observers_are_batch_safe_true_without_observers() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);
            push_token_triggers(&mut state, src, insect_token_effect(), None, 5);
            let run_len = batch_run_len(&state).unwrap();
            assert_eq!(run_len, 5);
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            let plan = effects::try_resolve_batch(&state, &ability, run_len).unwrap();
            assert!(observers_are_batch_safe(&state, &plan));
        }

        // §9.4a — Cathars'-class creature-ETB observer forces refusal + the
        // sequential fall-back produces the DESCENDING per-token distribution.
        #[test]
        fn creature_etb_observer_forces_sequential_descending_counters() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);

            // Cathars'-class observer: "Whenever a creature you control enters,
            // put a +1/+1 counter on each creature you control."
            let observer_id = create_object(
                &mut state,
                CardId(902),
                PlayerId(0),
                "Cathars' Crusade".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&observer_id).unwrap();
                obj.card_types.core_types.push(CoreType::Enchantment);
                let put_all = Effect::PutCounterAll {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }),
                };
                let trig = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        put_all,
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(trig.clone());
                obj.trigger_definitions.push(trig);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            push_token_triggers(&mut state, src, insect_token_effect(), None, 5);

            // Layer C refuses.
            {
                let run_len = batch_run_len(&state).unwrap();
                let ability = state.stack.back().unwrap().ability().unwrap().clone();
                let plan = effects::try_resolve_batch(&state, &ability, run_len).unwrap();
                assert!(
                    !observers_are_batch_safe(&state, &plan),
                    "creature-ETB observer must force refusal"
                );
            }

            // The batch driver must fall back to one entry at a time.
            let steps = resolve_to_empty_batched(&mut state);
            assert!(
                steps.iter().all(|&c| c == 1),
                "observer board must resolve one-at-a-time, got {steps:?}"
            );

            // CR 603.3: sequential interleaving — token1 (created first) is
            // present for the most subsequent Cathars resolutions, so its
            // +1/+1 counter total is the largest; the last token's is smallest.
            let mut totals: Vec<u32> = token_ids(&state)
                .iter()
                .map(|id| {
                    state.objects[id]
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        .copied()
                        .unwrap_or(0)
                })
                .collect();
            assert_eq!(totals.len(), 5);
            // The distribution is a strict descending permutation 5,4,3,2,1.
            totals.sort_unstable();
            assert_eq!(
                totals,
                vec![1, 2, 3, 4, 5],
                "descending fan-out per CR 603.3"
            );
        }

        // §9.5 — entering +1/+1 counter + live CounterAdded observer: the §2.2a
        // gate refuses BEFORE Layer C is consulted (try_resolve_batch == None),
        // and the sequential fall-back produces the descending distribution.
        #[test]
        fn entering_counter_with_counteradded_observer_refuses_and_falls_back() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);

            // CounterAdded observer: "Whenever one or more +1/+1 counters are put
            // on a creature you control, put a +1/+1 counter on each creature
            // you control." Registers under TriggerEventKey::CounterAdded ONLY —
            // NOT under any ETB/TokenCreated key.
            let observer_id = create_object(
                &mut state,
                CardId(903),
                PlayerId(0),
                "Counter Doubler".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&observer_id).unwrap();
                obj.card_types.core_types.push(CoreType::Enchantment);
                let put_all = Effect::PutCounterAll {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }),
                };
                let trig = TriggerDefinition::new(TriggerMode::CounterAdded)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        put_all,
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(trig.clone());
                obj.trigger_definitions.push(trig);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            // A Saproling token that enters WITH a +1/+1 counter.
            let mut saproling = insect_token_effect();
            if let Effect::Token {
                name,
                enter_with_counters,
                ..
            } = &mut saproling
            {
                *name = "Saproling".to_string();
                *enter_with_counters =
                    vec![(CounterType::Plus1Plus1, QuantityExpr::Fixed { value: 1 })];
            }
            push_token_triggers(&mut state, src, saproling, None, 5);

            // §2.2a: spec_emits_only_etb_pair == false ⇒ try_resolve_batch == None.
            {
                let run_len = batch_run_len(&state).unwrap();
                let ability = state.stack.back().unwrap().ability().unwrap().clone();
                assert!(
                    effects::try_resolve_batch(&state, &ability, run_len).is_none(),
                    "entering-counter spec must fail the §2.2a gate before Layer C"
                );
            }

            // Driver falls back to one-at-a-time.
            let steps = resolve_to_empty_batched(&mut state);
            assert!(
                steps.iter().all(|&c| c == 1),
                "entering-counter board must resolve sequentially, got {steps:?}"
            );

            // Each token entered with 1 counter; the CounterAdded observer then
            // fans out per token. token1 observes the most subsequent counter
            // events ⇒ descending distribution per CR 603.3.
            let mut totals: Vec<u32> = token_ids(&state)
                .iter()
                .map(|id| {
                    state.objects[id]
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        .copied()
                        .unwrap_or(0)
                })
                .collect();
            assert_eq!(totals.len(), 5);
            totals.sort_unstable();
            // Distinct strictly-descending per-token totals (not uniform).
            assert!(
                totals.windows(2).all(|w| w[0] < w[1]),
                "expected strict descending distribution, got {totals:?}"
            );
        }

        // §9.5 — Layer A/B predicate-shape refusals.
        #[test]
        fn non_fixed_count_is_not_batchable() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);
            let mut effect = insect_token_effect();
            if let Effect::Token { count, .. } = &mut effect {
                *count = QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Land],
                            ..Default::default()
                        }),
                    },
                };
            }
            push_token_triggers(&mut state, src, effect, None, 5);
            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            assert!(effects::try_resolve_batch(&state, &ability, run_len).is_none());
        }

        #[test]
        fn targeted_trigger_breaks_the_run() {
            let mut state = setup();
            let src = add_scute_source(&mut state);
            // A targeted Token (synthetic) is excluded by the empty-targets gate.
            for _ in 0..3 {
                let entry_id = ObjectId(state.next_object_id);
                state.next_object_id += 1;
                let ability = ResolvedAbility::new(
                    insect_token_effect(),
                    vec![TargetRef::Player(PlayerId(1))],
                    src,
                    PlayerId(0),
                );
                state.stack.push_back(StackEntry {
                    id: entry_id,
                    source_id: src,
                    controller: PlayerId(0),
                    kind: StackEntryKind::TriggeredAbility {
                        source_id: src,
                        ability: Box::new(ability),
                        condition: None,
                        trigger_event: None,
                        description: None,
                        source_name: String::new(),
                        subject_match_count: None,
                    },
                });
            }
            // Targeted entries are not batch candidates → no run key at top.
            assert!(batch_run_len(&state).is_none());
        }

        #[test]
        fn mixed_sources_form_a_contiguity_boundary() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src_a = add_scute_source(&mut state);
            let src_b = add_scute_source(&mut state);
            // Bottom: one trigger from src_b; top: 3 from src_a.
            push_token_triggers(&mut state, src_b, insect_token_effect(), None, 1);
            push_token_triggers(&mut state, src_a, insect_token_effect(), None, 3);
            // The contiguous run at the top is only the 3 src_a entries.
            assert_eq!(batch_run_len(&state), Some(3));
        }

        // §2.2a companion field exclusions.
        #[test]
        fn spec_emits_only_etb_pair_field_exclusions() {
            let base = TokenSpec {
                characteristics: crate::types::proposed_event::TokenCharacteristics {
                    display_name: "Insect".to_string(),
                    power: Some(1),
                    toughness: Some(1),
                    core_types: vec![CoreType::Creature],
                    subtypes: vec!["Insect".to_string()],
                    supertypes: vec![],
                    colors: vec![ManaColor::Green],
                    keywords: vec![],
                },
                script_name: "Insect".to_string(),
                static_abilities: vec![],
                enter_with_counters: vec![],
                tapped: false,
                enters_attacking: false,
                sacrifice_at: None,
                source_id: ObjectId(1),
                controller: PlayerId(0),
                attach_to: None,
            };
            // Bare spec passes.
            assert!(super::super::effects::token::spec_emits_only_etb_pair(
                &base
            ));

            let mut with_counter = base.clone();
            with_counter.enter_with_counters = vec![(CounterType::Plus1Plus1, 1)];
            assert!(!super::super::effects::token::spec_emits_only_etb_pair(
                &with_counter
            ));

            let mut attacking = base.clone();
            attacking.enters_attacking = true;
            assert!(!super::super::effects::token::spec_emits_only_etb_pair(
                &attacking
            ));

            let mut sac = base.clone();
            sac.sacrifice_at = Some(Duration::UntilEndOfCombat);
            assert!(!super::super::effects::token::spec_emits_only_etb_pair(
                &sac
            ));

            let mut attached = base.clone();
            attached.attach_to = Some(crate::game::game_object::AttachTarget::Object(ObjectId(2)));
            assert!(!super::super::effects::token::spec_emits_only_etb_pair(
                &attached
            ));
        }

        // §2.2 — ConditionInstead disjointness: a met copy-instead swap refuses.
        #[test]
        fn condition_instead_met_copy_branch_refuses() {
            let mut state = setup();
            add_lands(&mut state, 6); // 6 lands → "if you control 6+ lands" is met.
            let src = add_scute_source(&mut state);

            // sub: CopyTokenOf gated by ConditionInstead(lands >= 6).
            let copy_effect = Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            };
            let mut sub = ResolvedAbility::new(copy_effect, vec![], src, PlayerId(0));
            sub.condition = Some(AbilityCondition::ConditionInstead {
                inner: Box::new(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter {
                                type_filters: vec![TypeFilter::Land],
                                ..Default::default()
                            }),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 6 },
                }),
            });

            push_token_triggers(
                &mut state,
                src,
                insect_token_effect(),
                Some(Box::new(sub)),
                5,
            );
            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            // Condition met (6 lands) ⇒ swap to CopyTokenOf ⇒ not batchable in v1.
            assert!(effects::try_resolve_batch(&state, &ability, run_len).is_none());
        }

        // §2.2 — ConditionInstead NOT met + disjoint type ⇒ base Insect batches.
        #[test]
        fn condition_instead_not_met_disjoint_type_batches() {
            let mut state = setup();
            add_lands(&mut state, 3); // < 6 ⇒ base branch.
            let src = add_scute_source(&mut state);

            let copy_effect = Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            };
            let mut sub = ResolvedAbility::new(copy_effect, vec![], src, PlayerId(0));
            sub.condition = Some(AbilityCondition::ConditionInstead {
                inner: Box::new(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter {
                                type_filters: vec![TypeFilter::Land],
                                ..Default::default()
                            }),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 6 },
                }),
            });

            push_token_triggers(
                &mut state,
                src,
                insect_token_effect(),
                Some(Box::new(sub)),
                5,
            );
            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            // Land count invariant (token is a Creature, condition counts Lands) ⇒
            // base branch is provably stable ⇒ batchable.
            assert!(effects::try_resolve_batch(&state, &ability, run_len).is_some());
        }

        // §3.4 — mandatory Doubling-Season-class replacement still batches and
        // produces 2× tokens per resolution.
        #[test]
        fn mandatory_token_doubling_batches_and_doubles() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);

            // Doubling Season: mandatory token-count doubling replacement.
            let ds_id = create_object(
                &mut state,
                CardId(904),
                PlayerId(0),
                "Doubling Season".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&ds_id).unwrap();
                obj.card_types.core_types.push(CoreType::Enchantment);
                let repl = doubling_season_replacement();
                Arc::make_mut(&mut obj.base_replacement_definitions).push(repl.clone());
                obj.replacement_definitions.push(repl);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            push_token_triggers(&mut state, src, insect_token_effect(), None, 5);
            let steps = resolve_to_empty_batched(&mut state);
            assert_eq!(
                steps,
                vec![5],
                "mandatory replacement must not block batching"
            );
            // 5 resolutions × 2 (Doubling Season) = 10 Insect tokens.
            assert_eq!(token_ids(&state).len(), 10);
        }

        /// Push `n` identical untargeted Token triggers from `source_id`, each
        /// carrying the given entry-level intervening-if `condition` (CR 603.4).
        fn push_token_triggers_with_condition(
            state: &mut GameState,
            source_id: ObjectId,
            effect: Effect,
            condition: TriggerCondition,
            n: usize,
        ) {
            for _ in 0..n {
                let entry_id = ObjectId(state.next_object_id);
                state.next_object_id += 1;
                let ability = ResolvedAbility::new(effect.clone(), vec![], source_id, PlayerId(0));
                state.stack.push_back(StackEntry {
                    id: entry_id,
                    source_id,
                    controller: PlayerId(0),
                    kind: StackEntryKind::TriggeredAbility {
                        source_id,
                        ability: Box::new(ability),
                        condition: Some(condition.clone()),
                        trigger_event: None,
                        description: Some("Landfall".to_string()),
                        source_name: "Scute Swarm".to_string(),
                        subject_match_count: None,
                    },
                });
            }
        }

        // §9.5 HIGH (A4) — entry-level intervening-if that the run's OWN tokens
        // mutate (CR 603.4) MUST NOT batch. The condition reads the live creature
        // count; each created Insect raises it, so resolving the run one-by-one
        // stops firing once the threshold is crossed — producing FEWER tokens
        // than the run length. A batch that dropped the condition would fire all
        // N. This is the discriminating test for the dropped-`condition` defect.
        #[test]
        fn entry_intervening_if_over_run_mutated_count_does_not_batch() {
            let mut state = setup();
            // add_scute_source contributes ONE creature (the source). No lands
            // needed — the trigger entries are pushed directly.
            let src = add_scute_source(&mut state);

            // Intervening-if: "if you control fewer than 3 creatures, create a
            // 1/1 Insect". Each Insect raises the creature count the condition
            // reads — order-sensitive across the run.
            let condition = TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            ..Default::default()
                        }),
                    },
                },
                comparator: Comparator::LT,
                rhs: QuantityExpr::Fixed { value: 3 },
            };

            push_token_triggers_with_condition(
                &mut state,
                src,
                insect_token_effect(),
                condition,
                5,
            );

            // Layer A REFUSES to form a run: an entry carrying an entry-level
            // `condition` is non-batchable (it would become a singleton run that
            // falls back to `resolve_top`, which rechecks per entry per CR 603.4).
            assert!(
                batch_run_len(&state).is_none(),
                "an intervening-if entry must not start a batch run"
            );

            // Driver falls back to one-at-a-time for every entry.
            let steps = resolve_to_empty_batched(&mut state);
            assert!(
                steps.iter().all(|&c| c == 1),
                "intervening-if run must resolve one-at-a-time, got {steps:?}"
            );

            // Sequential semantics: baseline 1 creature (the source). Entry 1
            // sees 1<3 → +token (2). Entry 2 sees 2<3 → +token (3). Entries 3-5
            // see 3, not <3 → skip. Exactly 2 tokens — FEWER than the run of 5.
            // A reverted fix (condition dropped, all 5 batched) would give 5.
            assert_eq!(
                token_ids(&state).len(),
                2,
                "intervening-if must stop firing once the count crosses the threshold"
            );
        }

        // §9.5 HIGH-2 — produced-token-non-observer gate (direct, discriminating):
        // a produced token carrying an ETB observer trigger fails the gate, while
        // a landfall-only (EnterBattlefield(Some(Land))) trigger passes. This
        // exercises the gate's classifier directly — the copy path that would
        // surface such a trigger end-to-end always falls back wholesale anyway
        // (the met copy-instead branch, asserted below).
        #[test]
        fn produced_token_non_observer_gate_discriminates() {
            use super::super::effects::token::produced_token_is_non_observer;
            // A creature-ETB observer trigger ⇒ NOT a valid produced-token trigger.
            let etb_observer = TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    ..Default::default()
                }));
            assert!(
                !produced_token_is_non_observer(std::slice::from_ref(&etb_observer)),
                "an ETB-observing produced token must fail the gate"
            );
            // A landfall trigger (registers under EnterBattlefield(Some(Land))) is
            // STILL an EnterBattlefield key ⇒ conservatively rejected.
            let landfall = TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Land],
                    ..Default::default()
                }));
            assert!(
                !produced_token_is_non_observer(std::slice::from_ref(&landfall)),
                "any EnterBattlefield-keyed trigger is conservatively rejected"
            );
            // No triggers ⇒ passes (the bare Insect/Servo go-wide case).
            assert!(
                produced_token_is_non_observer(&[]),
                "a trigger-free produced token passes the gate"
            );
        }

        // §9.5 HIGH-2 — produced-token-non-observer gate: a CopyTokenOf run whose
        // copy SOURCE carries an ETB observer trigger must refuse. (The copy
        // branch falls back wholesale in v1 — see B5 — so this confirms a
        // copy-source observer never reaches a batched resolution.)
        #[test]
        fn copy_source_with_etb_observer_refuses_to_batch() {
            let mut state = setup();
            add_lands(&mut state, 3); // < 6 ⇒ base/copy decision routes to copy below.

            // A copy SOURCE permanent that itself carries a creature-ETB observer
            // trigger ("whenever a creature you control enters, ..."). Copies of
            // it would inherit this trigger and observe their siblings.
            let copy_source = create_object(
                &mut state,
                CardId(905),
                PlayerId(0),
                "Observer Source".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&copy_source).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                let etb_observer = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(etb_observer.clone());
                obj.trigger_definitions.push(etb_observer);
            }

            let src = add_scute_source(&mut state);

            // sub: CopyTokenOf gated by a MET ConditionInstead (lands >= 1) so the
            // copy branch is selected, then assert refusal (copy path falls back
            // wholesale in v1 — so a copy-source observer never batches).
            let copy_effect = Effect::CopyTokenOf {
                target: TargetFilter::SpecificObject { id: copy_source },
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            };
            let mut sub = ResolvedAbility::new(copy_effect, vec![], src, PlayerId(0));
            sub.condition = Some(AbilityCondition::ConditionInstead {
                inner: Box::new(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter {
                                type_filters: vec![TypeFilter::Land],
                                ..Default::default()
                            }),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                }),
            });

            push_token_triggers(
                &mut state,
                src,
                insect_token_effect(),
                Some(Box::new(sub)),
                5,
            );
            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            // The instead-swap fires (>= 1 land) ⇒ copy branch ⇒ not batchable in
            // v1 (the copy path produces no TokenSpec and falls back). The gate
            // therefore refuses regardless — confirming a copy-source observer
            // never reaches a batched resolution.
            assert!(
                effects::try_resolve_batch(&state, &ability, run_len).is_none(),
                "copy branch (and any copy-source observer) must refuse to batch"
            );
        }

        // §9.5 MEDIUM-1 — interactive/optional replacement gate: an OPTIONAL
        // token-doubling replacement applicable to the produced token must refuse
        // (token_creation_needs_choice == true). The mandatory positive control is
        // covered by `mandatory_token_doubling_batches_and_doubles`.
        #[test]
        fn optional_replacement_refuses_to_batch() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);

            // An OPTIONAL ("you may") token-count-doubling replacement.
            let opt_id = create_object(
                &mut state,
                CardId(906),
                PlayerId(0),
                "Optional Doubler".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&opt_id).unwrap();
                obj.card_types.core_types.push(CoreType::Enchantment);
                let repl = optional_token_doubling_replacement();
                Arc::make_mut(&mut obj.base_replacement_definitions).push(repl.clone());
                obj.replacement_definitions.push(repl);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            push_token_triggers(&mut state, src, insect_token_effect(), None, 5);
            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            // The optional replacement could pause for a NeedsChoice prompt
            // mid-batch ⇒ Layer B refuses.
            assert!(
                effects::try_resolve_batch(&state, &ability, run_len).is_none(),
                "optional replacement must force fall-back"
            );
        }

        // CR 603.3 (HIGH-2 loophole regression) — the run's OWN source carries a
        // SECOND observer trigger keyed on the produced token's creature-ETB
        // (e.g. "Whenever a land enters, create a 1/1 creature token. Whenever a
        // creature enters, draw a card."). Under CR 603.3 each token-creation and
        // each observer firing goes on the stack one at a time, with priority in
        // between, so batching ("all tokens, then all observers") would skip the
        // priority interleaving and let a player act between resolutions. Layer C
        // MUST refuse. This test would have FALSELY PASSED (batch wrongly allowed)
        // when `observers_are_batch_safe` excluded the run's own source IDs: the
        // creature-ETB candidate == the run source, so the old `run_source_ids`
        // exclusion filtered it out and reported the run batch-safe. With the
        // exclusion removed, any registered observer — including the source's own
        // second trigger — forces sequential resolution.
        #[test]
        fn source_with_own_token_etb_observer_forces_refusal() {
            let mut state = setup();
            add_lands(&mut state, 3);
            // `add_scute_source` registers the LAND-ETB token-creating trigger
            // (the run). It never self-matches the creature-token probe.
            let src = add_scute_source(&mut state);

            // Attach a SECOND trigger to the SAME source, keyed on creature-ETB —
            // exactly the produced token's type. This is the loophole gemini
            // flagged: the run source observing its own produced tokens.
            {
                let obj = state.objects.get_mut(&src).unwrap();
                let creature_observer = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(creature_observer.clone());
                obj.trigger_definitions.push(creature_observer);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            push_token_triggers(&mut state, src, insect_token_effect(), None, 5);

            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            let plan = effects::try_resolve_batch(&state, &ability, run_len).unwrap();
            // The creature-ETB candidate IS the run source `src`. Pre-fix, the
            // exclusion dropped it and this assertion would FAIL (batch allowed);
            // post-fix it must hold (refuse to batch).
            assert!(
                !observers_are_batch_safe(&state, &plan),
                "run source's own token-ETB observer must force sequential resolution (CR 603.3)"
            );

            // End-to-end: the batch driver must fall back to one entry at a time.
            let steps = resolve_to_empty_batched(&mut state);
            assert!(
                steps.iter().all(|&c| c == 1),
                "source-observed run must resolve one-at-a-time, got {steps:?}"
            );
        }

        // §9.4a HIGH-1 regression — a live non-run battlefield observer keyed on a
        // NARROW non-Creature ETB subtype (artifact creature) that the produced
        // token matches must force Layer C to refuse. A round-2-style fixed
        // `Some(Creature)` probe would have MISSED the `Some(Artifact)` bucket.
        #[test]
        fn narrow_artifact_etb_observer_forces_refusal() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);

            // Observer narrowed to ARTIFACT ETB: registers ONLY under
            // EnterBattlefield(Some(Artifact)) — NOT under (Some(Creature)).
            let observer_id = create_object(
                &mut state,
                CardId(907),
                PlayerId(0),
                "Artifact Watcher".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&observer_id).unwrap();
                obj.card_types.core_types.push(CoreType::Enchantment);
                let trig = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Artifact],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(trig.clone());
                obj.trigger_definitions.push(trig);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            // The produced token is an ARTIFACT CREATURE (core_types = [Artifact,
            // Creature]) — so the Layer C probe builds a record whose core_types
            // include Artifact and hits the narrow observer's bucket.
            let mut servo = insect_token_effect();
            if let Effect::Token { name, types, .. } = &mut servo {
                *name = "Servo".to_string();
                *types = vec!["Artifact".to_string(), "Creature".to_string()];
            }
            push_token_triggers(&mut state, src, servo, None, 5);

            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            let plan = effects::try_resolve_batch(&state, &ability, run_len).unwrap();
            assert!(
                !observers_are_batch_safe(&state, &plan),
                "narrow artifact-ETB observer must force refusal (Some(Artifact) bucket)"
            );
        }

        // §9.4a — Kodama-class broad-ETB observer (valid_card = Permanent) keyed
        // under EnterBattlefield(None) must force Layer C to refuse. Documents
        // that the motivating /tmp/gamestate.json board (with Kodama) does NOT
        // batch (§2.4).
        #[test]
        fn kodama_broad_permanent_etb_observer_forces_refusal() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);

            // Broad permanent-ETB observer ("whenever another permanent you
            // control enters, ..."): valid_card = Permanent narrows to None ⇒
            // registers under the broad EnterBattlefield(None) key.
            let observer_id = create_object(
                &mut state,
                CardId(908),
                PlayerId(0),
                "Kodama of the East Tree".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&observer_id).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                let trig = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Permanent],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(trig.clone());
                obj.trigger_definitions.push(trig);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            push_token_triggers(&mut state, src, insect_token_effect(), None, 5);

            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            let plan = effects::try_resolve_batch(&state, &ability, run_len).unwrap();
            assert!(
                !observers_are_batch_safe(&state, &plan),
                "broad permanent-ETB observer must force refusal (None bucket)"
            );
        }

        // §9.4b / §9.2 — ConditionInstead DIFFERENTIAL harness: run BOTH the
        // not-met (batches) and met (falls back) cases through the real pipeline
        // and assert each produces the correct final state vs the sequential path.
        #[test]
        fn condition_instead_differential_not_met_and_met() {
            // Build a Scute-Swarm-style state with `lands` lands and 5 landfall
            // Token-or-copy triggers; return it ready to resolve.
            let build = |lands: usize| -> GameState {
                let mut state = setup();
                add_lands(&mut state, lands);
                let src = add_scute_source(&mut state);
                let copy_effect = Effect::CopyTokenOf {
                    target: TargetFilter::SelfRef,
                    owner: TargetFilter::Controller,
                    source_filter: None,
                    enters_attacking: false,
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    extra_keywords: vec![],
                    additional_modifications: vec![],
                };
                let mut sub = ResolvedAbility::new(copy_effect, vec![], src, PlayerId(0));
                sub.condition = Some(AbilityCondition::ConditionInstead {
                    inner: Box::new(AbilityCondition::QuantityCheck {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount {
                                filter: TargetFilter::Typed(TypedFilter {
                                    type_filters: vec![TypeFilter::Land],
                                    ..Default::default()
                                }),
                            },
                        },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 6 },
                    }),
                });
                push_token_triggers(
                    &mut state,
                    src,
                    insect_token_effect(),
                    Some(Box::new(sub)),
                    5,
                );
                state
            };

            // NOT met (3 lands): base Insect branch ⇒ batches; final state equals
            // sequential. Disjoint type (token is Creature, condition counts Lands)
            // proves invariance.
            {
                let base = build(3);
                let mut batched = base.clone();
                let mut sequential = base.clone();
                let steps = resolve_to_empty_batched(&mut batched);
                resolve_to_empty_sequential(&mut sequential);
                assert_eq!(
                    steps,
                    vec![5],
                    "not-met disjoint case must batch in one step"
                );
                assert_eq!(
                    token_ids(&batched).len(),
                    token_ids(&sequential).len(),
                    "batched token count must equal sequential"
                );
                assert_eq!(token_ids(&batched).len(), 5);
                assert_eq!(batched.battlefield.len(), sequential.battlefield.len());
            }

            // MET (6 lands): copy-instead fires ⇒ Layer B refuses ⇒ falls back to
            // sequential; final state still correct (5 copies, one per step).
            {
                let mut state = build(6);
                let steps = resolve_to_empty_batched(&mut state);
                assert!(
                    steps.iter().all(|&c| c == 1),
                    "met copy-instead must fall back one-at-a-time, got {steps:?}"
                );
                assert_eq!(
                    token_ids(&state).len(),
                    5,
                    "5 copy-token resolutions produce 5 tokens"
                );
            }
        }

        /// Build an OPTIONAL token-count-doubling replacement ("you may create
        /// twice that many tokens instead").
        fn optional_token_doubling_replacement() -> crate::types::ability::ReplacementDefinition {
            use crate::types::ability::{
                QuantityModification, ReplacementDefinition, ReplacementMode,
            };
            use crate::types::replacements::ReplacementEvent;
            let mut def = ReplacementDefinition::new(ReplacementEvent::CreateToken);
            def.mode = ReplacementMode::Optional { decline: None };
            def.quantity_modification = Some(QuantityModification::Double);
            def
        }

        /// Build a mandatory token-count-doubling replacement definition
        /// (Doubling Season's "create twice that many tokens instead").
        fn doubling_season_replacement() -> crate::types::ability::ReplacementDefinition {
            use crate::types::ability::{
                QuantityModification, ReplacementDefinition, ReplacementMode,
            };
            use crate::types::replacements::ReplacementEvent;
            let mut def = ReplacementDefinition::new(ReplacementEvent::CreateToken);
            def.mode = ReplacementMode::Mandatory;
            def.quantity_modification = Some(QuantityModification::Double);
            def
        }
    }
}
