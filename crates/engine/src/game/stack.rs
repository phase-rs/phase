use crate::types::ability::{
    ContinuousModification, Duration, Effect, EffectKind, KeywordAction, ResolvedAbility,
    TargetFilter, TargetRef,
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

pub(crate) fn restore_alternative_spell_normal_face(state: &mut GameState, object_id: ObjectId) {
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
    let paid_snapshot = state.stack_paid_facts.remove(&entry.id);

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
        die_result,
        ..
    } = entry.kind
    {
        state.current_trigger_match_count = subject_match_count;
        // CR 706.2 + CR 706.4 + CR 603.12: re-stamp the carried die-roll result
        // into resolution scope so a reflexive "When you do … the result"
        // sub-ability resolving on its own stack entry (a later apply(), after
        // the original roll's resolution scope cleared) reads the rolled value
        // via the `QuantityRef::EventContextAmount` cascade.
        state.die_result_this_resolution = die_result;
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
    let resolution_start_phase = state.phase;

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
                    if casting_variant.restores_front_face_after_stack_exit() {
                        restore_alternative_spell_normal_face(state, entry.id);
                    }
                }
                events.push(GameEvent::StackResolved {
                    object_id: entry.id,
                });
                state.current_trigger_event = None;
                state.current_trigger_events.clear();
                state.current_trigger_match_count = None;
                // CR 706.2 + CR 706.4: clear the carried die-roll result at the
                // same cross-resolution boundary as the batched subject count.
                state.die_result_this_resolution = None;
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

    // CR 702.88a: Rebound — on-resolve hook. If the resolving spell is a
    // non-permanent spell that carries `Keyword::Rebound`, was cast from
    // its owner's hand, and is not a token, push the next-upkeep delayed
    // triggered ability that offers an optional free recast and override
    // the destination from graveyard to exile.
    // CR 704.5d: tokens cease to exist off the battlefield (gate `!is_token`).
    // CR 603.7a: delayed triggered abilities are created during resolution.
    // CR 603.7d: source of the delayed trigger IS the resolving spell.
    // CR 608.2n: default destination for a resolved instant/sorcery is graveyard.
    // CR 702.88c: multiple instances of rebound on the same spell are
    // redundant — `has_keyword` returns true even if duplicates exist, so
    // arming runs at most once per resolution.
    let rebound_armed = if is_spell && !is_permanent_spell(state, entry.id) {
        let has_rebound = state.objects.get(&entry.id).is_some_and(|o| {
            !o.is_token
                && super::keywords::has_keyword(o, &crate::types::keywords::Keyword::Rebound)
        });
        if has_rebound && super::casting::spell_cast_origin(state, entry.id) == Some(Zone::Hand) {
            super::effects::rebound::arm_rebound(state, entry.id, entry.controller)
        } else {
            false
        }
    } else {
        false
    };

    // CR 608.3: Determine destination zone for spells.
    if is_spell {
        let end_procedure_exiles_resolving_object = ability.as_ref().is_some_and(|ability| {
            matches!(ability.effect, Effect::EndTheTurn)
                || (matches!(ability.effect, Effect::EndCombatPhase)
                    && resolution_start_phase.is_combat())
        });
        let dest = if end_procedure_exiles_resolving_object {
            // CR 724.1b / CR 724.2b: The "end the turn" and "end the combat
            // phase" procedures exile every object on the stack, including the
            // resolving object that `resolve_top` already popped before
            // executing its effect.
            Zone::Exile
        } else if paradigm_armed {
            // CR 702.xxx: Paradigm-armed spell exiles instead of going to
            // graveyard. The ExileLink is already created by arm_paradigm.
            Zone::Exile
        } else if rebound_armed {
            // CR 702.88a: Rebound-armed non-permanent spell exiles instead
            // of going to graveyard — the delayed trigger is already
            // queued by `arm_rebound`.
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
            // CR 712.14a + CR 310.11b: If this spell was finalized from an
            // ExileWithAltCost permission with `cast_transformed`, the permanent
            // enters the battlefield transformed (resolving to its back face).
            // The finalized stack-paid snapshot is authoritative here; the
            // mutable permission list is casting-time authorization, not
            // resolution-time cast metadata.
            if let Some(obj) = state.objects.get(&entry.id) {
                if paid_snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.cast_transformed)
                {
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

            // CR 702.176a: Impending — seed the N time counters into the ZoneChange
            // ProposedEvent BEFORE the replacement pipeline so Doubling Season and
            // similar counter-doubling replacements (CR 614.1a) can modify them.
            // N is read from the `Keyword::Impending { counters, .. }` on the still-
            // stack-resident object; `cast_variant_paid = Impending` is already stamped
            // by `finalize_cast_to_stack` in `casting_costs.rs`.
            if casting_variant == CastingVariant::Impending {
                let impending_counters = state.objects.get(&entry.id).and_then(|obj| {
                    obj.keywords.iter().find_map(|k| match k {
                        crate::types::keywords::Keyword::Impending { counters, .. } => {
                            Some(*counters)
                        }
                        _ => None,
                    })
                });
                if let Some(n) = impending_counters {
                    if n > 0 {
                        if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                            enter_with_counters,
                            ..
                        } = &mut proposed
                        {
                            enter_with_counters.push((CounterType::Time, n));
                        }
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
                            // CR 702.146b / CR 702.162a + CR 712.11a + CR
                            // 712.13: Disturb and MTMTE put the spell on the
                            // stack with its back face up. A resolving DFC
                            // spell becomes a permanent with the same face up;
                            // mark the battlefield object transformed without
                            // swapping faces again.
                            if matches!(
                                casting_variant,
                                CastingVariant::MoreThanMeetsTheEye | CastingVariant::Disturb
                            ) && to == Zone::Battlefield
                            {
                                let mut marked = false;
                                if let Some(obj) = state.objects.get_mut(&object_id) {
                                    if obj.back_face.is_some() && !obj.transformed {
                                        obj.transformed = true;
                                        marked = true;
                                    }
                                }
                                if marked {
                                    crate::game::layers::mark_layers_full(state);
                                    events.push(GameEvent::Transformed { object_id });
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
                    // CR 706.2 + CR 706.4: clear the carried die-roll result at
                    // the same cross-resolution boundary as the batched subject
                    // count.
                    state.die_result_this_resolution = None;
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

        // CR 400.7 + CR 712.11a: face-swapped stack spells revert to front
        // face when leaving the stack unless they resolved as that face onto
        // the battlefield.
        if casting_variant.restores_front_face_after_stack_exit()
            && !spell_in_zone(state, entry.id, Zone::Battlefield)
        {
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

            // CR 702.176a: Impending-cast permanent gets the `cast_variant_paid`
            // tag re-applied after `reset_for_battlefield_entry` cleared it.
            // The "not a creature" layer fixup and the end-step counter-removal
            // trigger both gate on this marker being present.
            if casting_variant == CastingVariant::Impending {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Impending,
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
    // CR 706.2 + CR 706.4: clear the carried die-roll result at the same
    // cross-resolution boundary as the batched subject count.
    state.die_result_this_resolution = None;

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
                    // Gather the run's per-entry source ids (top-down resolution
                    // order) so the met-copy prefix path can read each entry's
                    // `SelfRef` copy source. Only the top `run_len` contiguous
                    // batch-key-equal entries form the run. This allocates only
                    // on the batch-eligible path (run_len >= 2), never on the
                    // single-resolution hot path.
                    let run_source_ids: Vec<ObjectId> = state
                        .stack
                        .iter()
                        .rev()
                        .take(run_len as usize)
                        .map(|e| e.source_id)
                        .collect();
                    if let Some(plan) =
                        effects::try_resolve_batch(state, &ability, run_len, &run_source_ids)
                    {
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
///
/// `consumed` equals the full run length for the base-token path, but the
/// copy-prefix path (CR 707.2) may consume a value-equal PREFIX shorter than
/// the run — the divergent tail resolves in a subsequent `resolve_next` step.
///
/// CR 603.4: This path does NOT bump `ability_resolutions_this_turn`. A
/// resolution-count-dependent intervening-if lives as an entry-level condition,
/// and `batch_run_key` refuses any entry with `condition.is_some()`, so no
/// batched run can carry a `NthResolutionThisTurn`-gated condition that the
/// missing counter bump would desynchronize.
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
            die_result,
            ..
        } = &top.kind
        {
            state.current_trigger_event = Some(te.clone());
            state.current_trigger_events = vec![te.clone()];
            state.current_trigger_match_count = *subject_match_count;
            // CR 706.2 + CR 706.4 + CR 603.12: re-stamp the carried die-roll
            // result into resolution scope for a reflexive "When you do … the
            // result" sub-ability (see `resolve_top`).
            state.die_result_this_resolution = *die_result;
        }
    }

    // CR 608.2: Apply the effect N times through the existing per-resolution body.
    plan.execute(state, ability, events);

    // CR 603.7c: Clear trigger context after resolution completes.
    state.current_trigger_event = None;
    state.current_trigger_events.clear();
    state.current_trigger_match_count = None;
    // CR 706.2 + CR 706.4: clear the carried die-roll result at the same
    // cross-resolution boundary as the batched subject count.
    state.die_result_this_resolution = None;

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
        co_departed: Vec::new(),
    }
}

/// CR 111.2 + CR 109.4: The run-identity axis along the source dimension. A
/// base token's characteristics and controller are fixed at creation and do not
/// read the creating source, so triggers from DISTINCT sources are
/// resolution-identical and collapse under `SourceIndependent`. Any
/// source-relative effect (a copy that reads its own `SelfRef` source, an
/// attacking/attached token, a source-relative count) keeps a per-source
/// boundary via `Source(id)` so two sources never collapse incorrectly.
#[derive(PartialEq)]
enum BatchSourceAxis {
    SourceIndependent,
    Source(ObjectId),
}

/// Resolution-grade run key (stricter than the display `StackGroupKey`, §4.1).
/// Two adjacent entries join a run iff every field is equal AND the entry is an
/// untargeted `TriggeredAbility` (Layer A). Keyed on `source_axis` + deep-equal
/// `ResolvedAbility` (not display `source_name`), with the flattened target
/// vector required empty (CR 608.2b).
struct BatchRunKey<'a> {
    controller: PlayerId,
    source_axis: BatchSourceAxis,
    ability: &'a ResolvedAbility,
    description: Option<&'a str>,
    paid: Option<&'a StackPaidSnapshot>,
    trigger_event: Option<&'a GameEvent>,
}

/// CR 111.2 + CR 109.4: `ResolvedAbility` embeds `source_id` (and nested sub/
/// else abilities embed their own), so a derived `PartialEq` would treat two
/// otherwise-identical abilities from distinct sources as unequal — defeating
/// the `SourceIndependent` collapse. When both keys are `SourceIndependent` the
/// effect provably reads nothing from the source, so abilities are compared
/// with `source_id` canonicalized away (recursively, on the chain). When either
/// key is `Source(id)`, the per-source boundary already differs, so the regular
/// deep equality (including `source_id`) applies.
impl PartialEq for BatchRunKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        if self.controller != other.controller
            || self.source_axis != other.source_axis
            || self.description != other.description
            || self.paid != other.paid
            || self.trigger_event != other.trigger_event
        {
            return false;
        }
        match (&self.source_axis, &other.source_axis) {
            (BatchSourceAxis::SourceIndependent, BatchSourceAxis::SourceIndependent) => {
                abilities_equal_ignoring_source(self.ability, other.ability)
            }
            _ => self.ability == other.ability,
        }
    }
}

/// Compare two resolved abilities for batch-run identity while ignoring the
/// source-object id at every level of the sub/else chain. Cheap clone+normalize
/// only runs on the batch-eligible path. The classifier guarantees the effect
/// reads nothing else from the source, so source-id is the only field allowed
/// to differ across a `SourceIndependent` run.
fn abilities_equal_ignoring_source(a: &ResolvedAbility, b: &ResolvedAbility) -> bool {
    normalize_ability_source(a) == normalize_ability_source(b)
}

/// Clone an ability with `source_id` (and nested sub/else `source_id`s)
/// canonicalized to `ObjectId(0)`, so equality ignores the creating source.
fn normalize_ability_source(ability: &ResolvedAbility) -> ResolvedAbility {
    let mut out = ability.clone();
    out.source_id = ObjectId(0);
    out.sub_ability = out
        .sub_ability
        .map(|sub| Box::new(normalize_ability_source(&sub)));
    out.else_ability = out
        .else_ability
        .map(|alt| Box::new(normalize_ability_source(&alt)));
    out
}

/// Build the run key for an entry, or `None` if the entry is not a candidate
/// for batch-resolution (Layer A.1/A.4/A.5: must be an untargeted
/// `TriggeredAbility` with no entry-level intervening-if condition).
///
/// No-wildcard discipline: every field of the `TriggeredAbility` variant is
/// destructured explicitly (no `..`) so each is consciously dispositioned —
/// the same exhaustiveness the codebase mandates for match arms, applied to
/// struct destructuring. Field-by-field audit:
/// - `source_id`   — IN KEY via `source_axis` (CR 111.2 + CR 109.4). A base
///   token reads nothing from its source, so `token_effect_is_source_independent`
///   maps it to `SourceIndependent`, collapsing a run across DISTINCT sources
///   (the Scute Swarm O(N²)→O(N) fix). Any source-relative effect maps to
///   `Source(source_id)`, keeping the per-source boundary so two sources never
///   collapse incorrectly.
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
/// - `die_result` — EXCLUDED for the same reason as `subject_match_count`: it
///   is CR 706.2 resolution data (the carried die-roll result re-stamped from
///   the run's top entry in `resolve_batched`), not run identity. Keying on it
///   would needlessly split runs without changing correctness.
fn batch_run_key<'a>(state: &'a GameState, entry: &'a StackEntry) -> Option<BatchRunKey<'a>> {
    let StackEntryKind::TriggeredAbility {
        source_id,
        ability,
        condition,
        trigger_event,
        description,
        source_name: _,
        subject_match_count: _,
        die_result: _,
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
    // CR 111.2 + CR 109.4: collapse the source dimension when the base effect
    // reads nothing from the source (a base token's controller/characteristics
    // are fixed at creation), so distinct sources join one run. Otherwise keep
    // a per-source boundary.
    let source_axis = if effects::token::token_effect_is_source_independent(ability) {
        BatchSourceAxis::SourceIndependent
    } else {
        BatchSourceAxis::Source(*source_id)
    };
    Some(BatchRunKey {
        controller: entry.controller,
        source_axis,
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
            face_down_profile: None,
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

    /// CR 724.1b: "end the turn" exiles every object on the stack, including
    /// the resolving spell itself. Discriminating against routing the source
    /// through the normal CR 608.2n instant/sorcery graveyard path.
    #[test]
    fn end_the_turn_spell_exiles_resolving_object() {
        let mut state = setup();
        let spell_id = create_object(
            &mut state,
            CardId(724),
            PlayerId(0),
            "Time Stop".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(Effect::EndTheTurn, vec![], spell_id, PlayerId(0));

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(724),
                ability: Some(ability),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(state.objects[&spell_id].zone, Zone::Exile);
        assert!(state.exile.contains(&spell_id));
        assert!(!state.players[0].graveyard.contains(&spell_id));
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
                die_result: None,
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
                    resolution_cleanup: None,
                    duration: None,
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
                    die_result: None,
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
                die_result: None,
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
                die_result: None,
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
                    die_result: None,
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
                face_down_profile: None,
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
                face_down_profile: None,
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

        /// Create a plain creature permanent (no triggers/replacements) under
        /// player 0 with the given P/T and a single subtype, and return its id.
        /// Copy sources for the batch-copy path must be observer-free so the
        /// copy token inherits no ETB-keyed trigger (§2.3a). `name` doubles as
        /// the subtype so distinct names yield distinct copiable values.
        fn add_plain_creature_source(
            state: &mut GameState,
            name: &str,
            power: i32,
            toughness: i32,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(910),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&id).unwrap();
                obj.base_power = Some(power);
                obj.base_toughness = Some(toughness);
                obj.power = Some(power);
                obj.toughness = Some(toughness);
                obj.base_card_types = crate::types::card_type::CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Creature],
                    subtypes: vec![name.to_string()],
                };
                obj.card_types = obj.base_card_types.clone();
                obj.base_name = name.to_string();
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
            id
        }

        /// Create a real Scute-Swarm-shape copy source: a Creature carrying a
        /// landfall trigger ("Whenever a land enters under your control, ...")
        /// registered under `EnterBattlefield(Some(Land))` in BOTH the base and
        /// live trigger sets, so a CR 707.2 copy of it inherits the landfall
        /// trigger. `name` doubles as the subtype so distinct names yield
        /// distinct copiable values. Unlike `add_plain_creature_source`, the copy
        /// token is NOT observer-free — but its Land-keyed trigger does not
        /// observe its Creature siblings, so the refined §2.3a gate batches it.
        fn add_landfall_creature_source(
            state: &mut GameState,
            name: &str,
            power: i32,
            toughness: i32,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(912),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&id).unwrap();
                obj.base_power = Some(power);
                obj.base_toughness = Some(toughness);
                obj.power = Some(power);
                obj.toughness = Some(toughness);
                obj.base_card_types = crate::types::card_type::CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Creature],
                    subtypes: vec![name.to_string()],
                };
                obj.card_types = obj.base_card_types.clone();
                obj.base_name = name.to_string();
                // A landfall trigger keyed EnterBattlefield(Some(Land)) — the
                // actual Scute Swarm shape. It must live in base_trigger_definitions
                // so a copy (CR 707.2) inherits it.
                let landfall = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Land],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(landfall.clone());
                obj.trigger_definitions.push(landfall);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
            id
        }

        /// Create a copy source whose copied token would OBSERVE its in-batch
        /// siblings: a Creature carrying a "whenever a creature you control
        /// enters" trigger registered under `EnterBattlefield(Some(Creature))`.
        /// A CR 707.2 copy inherits it, and the copy's Creature emission DOES
        /// intersect the Creature ETB key, so the refined §2.3a gate must refuse.
        fn add_creature_observer_source(
            state: &mut GameState,
            name: &str,
            power: i32,
            toughness: i32,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(913),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&id).unwrap();
                obj.base_power = Some(power);
                obj.base_toughness = Some(toughness);
                obj.power = Some(power);
                obj.toughness = Some(toughness);
                obj.base_card_types = crate::types::card_type::CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Creature],
                    subtypes: vec![name.to_string()],
                };
                obj.card_types = obj.base_card_types.clone();
                obj.base_name = name.to_string();
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
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
            id
        }

        /// Build a `ConditionInstead`-gated `CopyTokenOf { target: SelfRef }` sub
        /// whose inner condition is "you control >= `threshold` Lands" — disjoint
        /// from the produced Creature copy's core types, so it stays H1-invariant.
        fn copy_instead_sub(src: ObjectId, threshold: i32) -> ResolvedAbility {
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
                    rhs: QuantityExpr::Fixed { value: threshold },
                }),
            });
            sub
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
                        die_result: None,
                    },
                });
            }
        }

        /// Push `n` identical untargeted Token triggers, EACH from a DISTINCT
        /// source object (mirrors a Scute-Swarm board where many copies each
        /// fire their own landfall trigger). Returns the created source ids in
        /// push order. Each source is a plain creature carrying a landfall
        /// trigger keyed on `EnterBattlefield(Some(Land))` — it never observes
        /// the creature-token probe, exactly like the single-source helper's
        /// `add_scute_source`.
        fn push_token_triggers_from_distinct_sources(
            state: &mut GameState,
            effect: Effect,
            sub_ability: Option<Box<ResolvedAbility>>,
            n: usize,
        ) -> Vec<ObjectId> {
            let mut sources = Vec::with_capacity(n);
            for _ in 0..n {
                let src = add_scute_source(state);
                push_token_triggers(state, src, effect.clone(), sub_ability.clone(), 1);
                sources.push(src);
            }
            sources
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

        /// Test shim: gather the top `run_len` run source ids and invoke the
        /// real `effects::try_resolve_batch`. Mirrors the gather `resolve_next`
        /// performs at the live call site so tests exercise the true signature.
        fn try_batch(
            state: &GameState,
            ability: &ResolvedAbility,
            run_len: u32,
        ) -> Option<effects::BatchPlan> {
            let run_source_ids: Vec<ObjectId> = state
                .stack
                .iter()
                .rev()
                .take(run_len as usize)
                .map(|e| e.source_id)
                .collect();
            effects::try_resolve_batch(state, ability, run_len, &run_source_ids)
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
            let plan = try_batch(&state, &ability, run_len).unwrap();
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
                let plan = try_batch(&state, &ability, run_len).unwrap();
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
                    try_batch(&state, &ability, run_len).is_none(),
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
            assert!(try_batch(&state, &ability, run_len).is_none());
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
                        die_result: None,
                    },
                });
            }
            // Targeted entries are not batch candidates → no run key at top.
            assert!(batch_run_len(&state).is_none());
        }

        /// CR 111.2 + CR 109.4: distinct base-token sources now JOIN one run —
        /// the source dimension collapses under `SourceIndependent` because a
        /// base token reads nothing from its creating source. A source-relative
        /// effect (e.g. enters-attacking) keeps a per-source boundary.
        #[test]
        fn mixed_sources_form_a_contiguity_boundary() {
            // Base token: source-independent ⇒ distinct sources JOIN.
            let mut state = setup();
            add_lands(&mut state, 3);
            let src_a = add_scute_source(&mut state);
            let src_b = add_scute_source(&mut state);
            // Bottom: one trigger from src_b; top: 3 from src_a — both base Insect.
            push_token_triggers(&mut state, src_b, insect_token_effect(), None, 1);
            push_token_triggers(&mut state, src_a, insect_token_effect(), None, 3);
            // CR 111.2/109.4: all 4 distinct-source base-token entries form one run.
            assert_eq!(
                batch_run_len(&state),
                Some(4),
                "base tokens from distinct sources must collapse into one run"
            );

            // Source-relative token (enters_attacking) ⇒ Source(id) boundary.
            let mut attacking_effect = insect_token_effect();
            if let Effect::Token {
                ref mut enters_attacking,
                ..
            } = attacking_effect
            {
                *enters_attacking = true;
            }
            let mut state2 = setup();
            add_lands(&mut state2, 3);
            let src_c = add_scute_source(&mut state2);
            let src_d = add_scute_source(&mut state2);
            // Bottom: one from src_d; top: 3 from src_c — source-relative.
            push_token_triggers(&mut state2, src_d, attacking_effect.clone(), None, 1);
            push_token_triggers(&mut state2, src_c, attacking_effect, None, 3);
            // Source-relative effect keeps a per-source boundary: only the top
            // 3 src_c entries form the run.
            assert_eq!(
                batch_run_len(&state2),
                Some(3),
                "source-relative tokens must keep a per-source boundary"
            );
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

        // §2.2 + CR 707.2 — ConditionInstead MET copy branch: a single
        // (identical-value) source's met copy-instead swap now BATCHES along the
        // value-equal prefix (whole run), consuming `run_len` entries.
        #[test]
        fn condition_instead_met_copy_branch_refuses() {
            let mut state = setup();
            add_lands(&mut state, 6); // 6 lands → "if you control 6+ lands" is met.
                                      // Observer-free source so the copy token passes the §2.3a gate.
            let src = add_plain_creature_source(&mut state, "Scout", 1, 1);
            let sub = copy_instead_sub(src, 6);

            push_token_triggers(
                &mut state,
                src,
                insect_token_effect(),
                Some(Box::new(sub)),
                5,
            );
            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            // Condition met (6 lands) ⇒ swap to CopyTokenOf. The single source's
            // 5 entries share identical copiable values (CR 707.2), so the copy
            // prefix collapses the whole run into one batch.
            let plan = try_batch(&state, &ability, run_len)
                .expect("met copy-instead with identical values must batch");
            assert_eq!(
                plan.consumed(),
                run_len,
                "identical-source copy prefix must consume the full run"
            );
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
            assert!(try_batch(&state, &ability, run_len).is_some());
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
                        die_result: None,
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
        // the gate is the INTERSECTION of a trigger's registered keys with the
        // produced token's CR 603.6a emission. A Creature produced token emits
        // exactly {EnterBattlefield(None), EnterBattlefield(Some(Creature)),
        // TokenCreated}. A creature-ETB observer intersects (refused); the real
        // Scute-shape landfall trigger (EnterBattlefield(Some(Land))) does NOT
        // intersect a creature emission and is batch-SAFE (the HIGH fix — the old
        // coarse wildcard gate refused this and the headline repro never batched).
        #[test]
        fn produced_token_non_observer_gate_discriminates() {
            use super::super::effects::token::produced_token_is_non_observer;
            // The produced (copied) token is a Creature: emission =
            // {None, Some(Creature), TokenCreated}.
            let produced_creature = [CoreType::Creature];

            // A creature-ETB observer trigger registers under Some(Creature) ⇒
            // intersects the creature emission ⇒ must fail the gate.
            let etb_observer = TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    ..Default::default()
                }));
            assert!(
                !produced_token_is_non_observer(
                    std::slice::from_ref(&etb_observer),
                    &produced_creature
                ),
                "a creature-ETB-observing produced token must fail the gate"
            );

            // The HEADLINE fix: a landfall trigger registers under
            // EnterBattlefield(Some(Land)). A Creature copy emits no Land key, so
            // the intersection is EMPTY ⇒ the Scute-shape copy is batch-SAFE. The
            // old coarse gate (any EnterBattlefield(_)) refused this and the named
            // repro never collapsed.
            let landfall = TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Land],
                    ..Default::default()
                }));
            assert!(
                produced_token_is_non_observer(std::slice::from_ref(&landfall), &produced_creature),
                "a Land-keyed landfall trigger on a Creature copy does not observe \
                 its creature siblings ⇒ batch-safe (the HIGH fix)"
            );

            // Over-permit guard: a broad permanent-ETB observer registers under
            // the broad EnterBattlefield(None) key, which is in EVERY token's
            // emission ⇒ must still be refused.
            let broad_etb = TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Permanent],
                    ..Default::default()
                }));
            assert!(
                !produced_token_is_non_observer(
                    std::slice::from_ref(&broad_etb),
                    &produced_creature
                ),
                "a broad permanent-ETB observer (None key) intersects every emission ⇒ refused"
            );

            // Symmetry check: the SAME landfall trigger on a LAND copy (emission
            // includes Some(Land)) DOES intersect ⇒ refused. Proves the gate keys
            // off the produced token's real core types, not a fixed assumption.
            assert!(
                !produced_token_is_non_observer(std::slice::from_ref(&landfall), &[CoreType::Land]),
                "a landfall trigger on a Land copy observes its land siblings ⇒ refused"
            );

            // No triggers ⇒ passes (the bare Insect/Servo go-wide case).
            assert!(
                produced_token_is_non_observer(&[], &produced_creature),
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
                try_batch(&state, &ability, run_len).is_none(),
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
                try_batch(&state, &ability, run_len).is_none(),
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
            let plan = try_batch(&state, &ability, run_len).unwrap();
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
            let plan = try_batch(&state, &ability, run_len).unwrap();
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
            let plan = try_batch(&state, &ability, run_len).unwrap();
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
                // Observer-free copy source so the met-copy branch can batch
                // (a copy inherits the source's triggers; an ETB-keyed trigger
                // would fail the §2.3a non-observer gate).
                let src = add_plain_creature_source(&mut state, "Scout", 1, 1);
                let sub = copy_instead_sub(src, 6);
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

            // MET (6 lands): copy-instead fires ⇒ Layer B copy-prefix batches.
            // The single source's 5 entries share identical copiable values
            // (CR 707.2), and the observer-free copy token passes §2.3a, so the
            // whole run collapses into ONE batched step producing 5 copies —
            // equal to the sequential path.
            {
                let base = build(6);
                let mut batched = base.clone();
                let mut sequential = base.clone();
                let steps = resolve_to_empty_batched(&mut batched);
                resolve_to_empty_sequential(&mut sequential);
                assert_eq!(
                    steps,
                    vec![5],
                    "met copy-instead with identical values must batch in one step, got {steps:?}"
                );
                assert_eq!(
                    token_ids(&batched).len(),
                    5,
                    "5 copy-token resolutions produce 5 tokens"
                );
                assert_eq!(
                    token_ids(&batched).len(),
                    token_ids(&sequential).len(),
                    "batched copy count must equal sequential"
                );
            }
        }

        // CR 111.2 + CR 109.4 — cross-source base-token collapse: K distinct
        // sources each fire one base Insect Token trigger. Because a base token
        // reads nothing from its source, the run-identity source axis is
        // `SourceIndependent` and all K entries form ONE batch (the Scute Swarm
        // O(N²)→O(N) fix). Result equals the sequential path.
        #[test]
        fn cross_source_base_token_forms_one_batch() {
            let mut base = setup();
            add_lands(&mut base, 3);
            let sources = push_token_triggers_from_distinct_sources(
                &mut base,
                insect_token_effect(),
                None,
                7,
            );
            assert_eq!(sources.len(), 7);

            let mut batched = base.clone();
            let mut sequential = base.clone();

            let steps = resolve_to_empty_batched(&mut batched);
            resolve_to_empty_sequential(&mut sequential);

            assert_eq!(
                steps,
                vec![7],
                "7 distinct-source base-token entries must collapse into one batch"
            );
            assert_eq!(token_ids(&batched).len(), 7);
            assert_eq!(token_ids(&sequential).len(), 7);
            assert_eq!(batched.battlefield.len(), sequential.battlefield.len());
        }

        // CR 707.2 — cross-source copy collapse: K distinct sources with
        // IDENTICAL copiable values each fire a met copy-instead self-copy. The
        // value-equal prefix spans the whole run, so all K collapse into one
        // batch producing K copies. Result equals the sequential path.
        #[test]
        fn cross_source_copy_identical_values_forms_one_batch() {
            let mut base = setup();
            add_lands(&mut base, 6); // met ⇒ copy branch fires.

            // K distinct, value-identical observer-free creature sources, each
            // firing a met copy-instead self-copy.
            for _ in 0..5 {
                let src = add_plain_creature_source(&mut base, "Clone Base", 2, 2);
                let sub = copy_instead_sub(src, 6);
                push_token_triggers(
                    &mut base,
                    src,
                    insect_token_effect(),
                    Some(Box::new(sub)),
                    1,
                );
            }

            let mut batched = base.clone();
            let mut sequential = base.clone();

            let steps = resolve_to_empty_batched(&mut batched);
            resolve_to_empty_sequential(&mut sequential);

            assert_eq!(
                steps,
                vec![5],
                "5 identical-value cross-source copies must collapse into one batch, got {steps:?}"
            );
            // 5 copy tokens, all copies of "Clone Base".
            let batched_copies: Vec<_> = token_ids(&batched)
                .into_iter()
                .filter(|id| batched.objects[id].name == "Clone Base")
                .collect();
            assert_eq!(batched_copies.len(), 5);
            assert_eq!(
                token_ids(&batched).len(),
                token_ids(&sequential).len(),
                "batched copy count must equal sequential"
            );
        }

        // CR 707.2 + CR 707.5 + CR 603.6a — THE HEADLINE Scute Swarm repro:
        // K distinct copy sources are real Scute-Swarm-shape creatures, each
        // carrying a landfall trigger keyed EnterBattlefield(Some(Land)). The
        // copied tokens are CREATURES that inherit the landfall trigger (CR
        // 707.2/707.5). A Creature copy emits {None, Some(Creature), TokenCreated}
        // — the Land-keyed landfall does NOT intersect it, so the §2.3a gate is
        // safe and the whole run STILL collapses into ONE batch. This is
        // DISCRIMINATING: under the OLD coarse gate (any EnterBattlefield(_)
        // rejected) try_resolve_copy_batch returned None and the run resolved
        // one-at-a-time — the named perf bug was never fixed for its own card.
        #[test]
        fn cross_source_copy_with_landfall_trigger_still_batches() {
            let mut base = setup();
            add_lands(&mut base, 6); // met ⇒ copy branch fires.

            // K distinct value-identical Scute-shape sources, each firing a met
            // copy-instead self-copy. Each source (and thus each copy) carries a
            // landfall trigger keyed on Land ETB — exactly Scute Swarm.
            for _ in 0..5 {
                let src = add_landfall_creature_source(&mut base, "Scute Swarm", 1, 1);
                let sub = copy_instead_sub(src, 6);
                push_token_triggers(
                    &mut base,
                    src,
                    insect_token_effect(),
                    Some(Box::new(sub)),
                    1,
                );
            }

            let mut batched = base.clone();
            let mut sequential = base.clone();

            let steps = resolve_to_empty_batched(&mut batched);
            resolve_to_empty_sequential(&mut sequential);

            assert_eq!(
                steps,
                vec![5],
                "the real Scute Swarm shape (landfall on a creature copy) MUST collapse \
                 into one batch — would be all-1 under the old coarse gate, got {steps:?}"
            );
            let batched_copies: Vec<_> = token_ids(&batched)
                .into_iter()
                .filter(|id| batched.objects[id].name == "Scute Swarm")
                .collect();
            assert_eq!(batched_copies.len(), 5, "5 Scute Swarm copies produced");
            assert_eq!(
                token_ids(&batched).len(),
                token_ids(&sequential).len(),
                "batched copy count must equal sequential"
            );
            // The copies carry the inherited landfall trigger (CR 707.2/707.5).
            for id in &batched_copies {
                assert!(
                    !batched.objects[id].trigger_definitions.is_empty(),
                    "the copy must inherit the source's landfall trigger"
                );
            }
        }

        // CR 603.6a (over-permit guard) — a SelfRef copy whose copied token DOES
        // observe its in-batch siblings must STILL refuse. The copy source is a
        // Creature carrying a "whenever a creature you control enters" trigger
        // (EnterBattlefield(Some(Creature))); the Creature copy's emission
        // includes Some(Creature), so the intersection is non-empty ⇒ refused.
        // Proves the refined gate did not become unsafe.
        #[test]
        fn cross_source_copy_with_creature_etb_observer_refuses_batch() {
            let mut state = setup();
            add_lands(&mut state, 6); // met ⇒ copy branch fires.

            for _ in 0..5 {
                let src = add_creature_observer_source(&mut state, "Watcher", 2, 2);
                let sub = copy_instead_sub(src, 6);
                push_token_triggers(
                    &mut state,
                    src,
                    insect_token_effect(),
                    Some(Box::new(sub)),
                    1,
                );
            }

            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            // The copied token observes creature ETB (its siblings) ⇒ the §2.3a
            // intersection is non-empty ⇒ must refuse to batch.
            assert!(
                try_batch(&state, &ability, run_len).is_none(),
                "a copy whose token observes creature-ETB siblings must refuse to batch"
            );
        }

        // CR 707.2 — divergent-tail prefix batching: K cross-source copies where
        // a middle source diverges in copiable values. The contiguous value-equal
        // PREFIX collapses; the divergent tail resolves in subsequent steps. The
        // step pattern proves prefix batching (not all-1, not one vec![K]), and
        // the final token count equals the sequential path.
        #[test]
        fn cross_source_copy_divergent_tail_batches_prefix_then_resolves_rest() {
            let mut base = setup();
            add_lands(&mut base, 6);

            // Push order (resolution order is top-down = LIFO): the LAST pushed
            // entry resolves first. Push the divergent source FIRST so it sits at
            // the BOTTOM and the value-equal sources are at the top.
            //
            // Build: 2 identical "Alpha" sources, then 1 "Beta" (divergent P/T),
            // then 2 more "Alpha". Pushed bottom→top. Resolution order (top→down):
            // Alpha, Alpha, Beta, Alpha, Alpha. The prefix is the top 2 Alphas.
            let specs: [(&str, i32, i32); 5] = [
                ("Alpha", 2, 2),
                ("Alpha", 2, 2),
                ("Beta", 3, 3),
                ("Alpha", 2, 2),
                ("Alpha", 2, 2),
            ];
            for (name, p, t) in specs {
                let src = add_plain_creature_source(&mut base, name, p, t);
                let sub = copy_instead_sub(src, 6);
                push_token_triggers(
                    &mut base,
                    src,
                    insect_token_effect(),
                    Some(Box::new(sub)),
                    1,
                );
            }

            let mut batched = base.clone();
            let mut sequential = base.clone();

            let steps = resolve_to_empty_batched(&mut batched);
            resolve_to_empty_sequential(&mut sequential);

            // The top 2 Alphas batch (prefix), then Beta resolves, then the
            // bottom 2 Alphas batch. NOT all-1 and NOT a single vec![5].
            assert_eq!(
                steps,
                vec![2, 1, 2],
                "prefix batching must collapse the value-equal head, got {steps:?}"
            );
            // 5 copy tokens total (3 Alpha + 1 Beta + ... by name), equal to
            // sequential.
            assert_eq!(token_ids(&batched).len(), 5);
            assert_eq!(
                token_ids(&batched).len(),
                token_ids(&sequential).len(),
                "batched count must equal sequential"
            );
        }

        // CR 608.2c (H1 discriminator) — a met copy that creates LANDS gated on a
        // LAND count must NOT batch: the copy's core types intersect the counted
        // type, so the intervening condition is order-sensitive across the run.
        // This FAILS if the invariance gate is fed the base placeholder core
        // types ([Creature]) and PASSES (refuses) when fed the COPY core types
        // ([Land]).
        #[test]
        fn met_copy_creating_lands_gated_on_land_count_refuses_batch() {
            let mut state = setup();
            add_lands(&mut state, 6); // met ⇒ copy branch fires.

            // Observer-free copy source whose copiable type is LAND (not the
            // base Insect Creature). Copying it produces Land tokens.
            let land_src = create_object(
                &mut state,
                CardId(911),
                PlayerId(0),
                "Mirror Land".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&land_src).unwrap();
                obj.base_card_types = crate::types::card_type::CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Land],
                    subtypes: vec!["Forest".to_string()],
                };
                obj.card_types = obj.base_card_types.clone();
                obj.base_name = "Mirror Land".to_string();
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            let sub = copy_instead_sub(land_src, 6);
            push_token_triggers(
                &mut state,
                land_src,
                insect_token_effect(),
                Some(Box::new(sub)),
                5,
            );

            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            // The copy creates Lands; the condition counts Lands ⇒ each created
            // Land flips the count ⇒ order-sensitive ⇒ must refuse.
            assert!(
                try_batch(&state, &ability, run_len).is_none(),
                "a met copy creating Lands gated on a Land count must refuse to batch"
            );
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

        // ====================================================================
        // Incremental layer-flush performance + correctness regression tests.
        // ====================================================================

        use crate::game::layers::{evaluate_layers, flush_layers, FULL_EVALUATE_LAYERS_COUNT};
        use std::sync::atomic::Ordering;

        /// (A) REAL-BOARD smoke test on the 583-Scute-Swarm `/tmp/gamestate.json`
        /// repro. Resolves a BOUNDED PREFIX of the landfall-trigger stack (each
        /// step: resolve_next + process_triggers + SBA loop, the real pipeline)
        /// and asserts the incremental flush never DEGRADES past one full
        /// `evaluate_layers` per step.
        ///
        /// NOTE: this fixture is NOT an O(N) board, and that is rules-correct. Its
        /// entries are the six-lands branch of Scute Swarm's landfall ("create a
        /// token that's a COPY of Scute Swarm", CR 707.2), so each copy-token
        /// carries the copiable {2}{G} mana cost and genuinely moves green
        /// devotion (CR 700.5). Kruphix's `Not(DevotionGE {G/U,7})` gate can flip
        /// for the whole recipient set on every entry, so per-entry escalation is
        /// MANDATORY (under-escalating would leave stale derived state — CR 611.3a,
        /// the #1 hard rule). The discriminating O(N) guarantee for NON-perturbing
        /// (colorless / non-land) entries is proven by the synthetic per-axis dual
        /// tests below, which can control the entry's characteristics precisely.
        ///
        /// Bounded to a prefix because the FULL 2,891-trigger resolution is
        /// dominated by the O(N²) trigger-scan / SBA pipeline (independent of the
        /// layers fix) and is impractically slow in a debug build.
        ///
        /// Self-skips when `/tmp/gamestate.json` is absent (CI lacks the repro).
        /// `#[ignore]` by default: depends on a local-only 27MB snapshot. Run with
        /// `cargo test -p engine -- --ignored real_scute_board`.
        /// Re-parse every `StaticCondition::Unrecognized` carried in a snapshot's
        /// static definitions through the live `parse_inner_condition`, replacing
        /// any that now parse to a typed condition. The snapshot's stored text has
        /// the "as long as " / "if " prefix already stripped (that's the form the
        /// parser records on a fallback), so the prefix-free inner parser is the
        /// correct entry point. Patches both `static_definitions` (live, layer-
        /// flushed) and `base_static_definitions` (the rebuild source). This makes
        /// a pre-fix snapshot reflect the parser change under test.
        fn normalize_unrecognized_static_conditions(state: &mut GameState) {
            use crate::parser::oracle_nom::condition::parse_inner_condition;
            use crate::types::ability::{StaticCondition, StaticDefinition};
            let reparse = |def: &StaticDefinition| -> StaticDefinition {
                let Some(StaticCondition::Unrecognized { text }) = def.condition.as_ref() else {
                    return def.clone();
                };
                match parse_inner_condition(text) {
                    Ok(("", parsed)) => {
                        let mut new_def = def.clone();
                        new_def.condition = Some(parsed);
                        new_def
                    }
                    _ => def.clone(),
                }
            };
            let ids: Vec<ObjectId> = state.objects.keys().copied().collect();
            for id in ids {
                let Some(obj) = state.objects.get_mut(&id) else {
                    continue;
                };
                let new_live: Vec<StaticDefinition> =
                    obj.static_definitions.iter_all().map(&reparse).collect();
                let new_base: Vec<StaticDefinition> =
                    obj.base_static_definitions.iter().map(&reparse).collect();
                obj.static_definitions = new_live.into();
                obj.base_static_definitions = Arc::new(new_base);
            }
        }

        #[test]
        #[ignore = "requires local /tmp/gamestate.json repro"]
        fn real_scute_board_resolution_is_not_full_eval_per_token() {
            let path = "/tmp/gamestate.json";
            let Ok(contents) = std::fs::read_to_string(path) else {
                eprintln!("skipping: {path} not present");
                return;
            };
            let wrapper: serde_json::Value =
                serde_json::from_str(&contents).expect("repro wrapper must parse");
            let gs_value = wrapper
                .get("gameState")
                .expect("wrapper must have gameState member")
                .clone();
            let mut state: GameState =
                serde_json::from_value(gs_value).expect("gameState must deserialize");

            // This repro was serialized BEFORE the Grist source-zone parser fix,
            // so Grist's "as long as ~ isn't on the battlefield" static is frozen
            // in the snapshot as `StaticCondition::Unrecognized` (which the
            // escalation classifier must treat as conservatively population-
            // sensitive → escalate every step). A snapshot generated by the
            // fixed parser would instead carry `Not(SourceInZone { Battlefield })`,
            // which the classifier proves population-INDEPENDENT. Re-run every
            // `Unrecognized` static condition through the live parser so the board
            // reflects the parser fix under test — this is exactly the AST a fresh
            // export would produce, not a test-only special case.
            normalize_unrecognized_static_conditions(&mut state);

            let stack_size = state.stack.len();
            assert!(
                stack_size > 100,
                "repro must have a large stack (got {stack_size})"
            );

            // First flush rebuilds fully (deserialized snapshot defaults to Full).
            // Reset the counter AFTER that initial mandatory full pass so we only
            // measure per-resolution behavior.
            flush_layers(&mut state);
            FULL_EVALUATE_LAYERS_COUNT.store(0, Ordering::Relaxed);

            const PREFIX_STEPS: usize = 120;
            let mut steps = 0usize;
            let resolve_start = std::time::Instant::now();
            while !state.stack.is_empty() && steps < PREFIX_STEPS {
                let mut events = Vec::new();
                resolve_next(&mut state, &mut events);
                triggers::process_triggers(&mut state, &events);
                crate::game::sba::check_state_based_actions(&mut state, &mut events);
                steps += 1;
            }
            let resolve_elapsed = resolve_start.elapsed();
            let full_evals = FULL_EVALUATE_LAYERS_COUNT.load(Ordering::Relaxed);
            eprintln!(
                "real-board probe: full_evals={full_evals} steps={steps} \
                 wall_clock={resolve_elapsed:?} ({:.1}ms/step)",
                resolve_elapsed.as_secs_f64() * 1000.0 / steps.max(1) as f64
            );

            assert!(
                steps > 20,
                "prefix must resolve enough steps to discriminate (got {steps})"
            );
            // CR 611.3a + CR 611.3b — TRUTH-DELTA SHORT-CIRCUIT: full evals on
            // this repro collapse to NEAR-CONSTANT (measured 4 across 120 steps,
            // down from ~63 before the short-circuit). The board carries board-
            // population-gated statics (Kruphix `Not(DevotionGE {G/U,7})`,
            // Anger/Brawn land-presence, Grist's source-zone gate). The entries
            // are the six-lands branch of Scute Swarm's landfall: "create a token
            // that's a COPY of Scute Swarm" (CR 707.2 — the copy takes the
            // copiable mana cost {2}{G}), so each copy-token carries a GREEN mana
            // symbol and CR 700.5 devotion to green strictly INCREASES on every
            // entry.
            //
            // Under d9a40be71 every such devotion-perturbing entry escalated to a
            // full pass (~1 per copy → ~63). But Kruphix's gate is
            // `Not(DevotionGE 7)`: once green devotion is already >= 7 (it is,
            // early), the gate TRUTH is stable FALSE and never flips again no
            // matter how high devotion climbs. The truth-delta short-circuit
            // recomputes the gate's AFTER truth against the live board and skips
            // escalation when `before == after` — so devotion-perturbing-but-
            // non-flipping entries now stay on the incremental fast path. The few
            // residual full evals are rules-MANDATORY flips (a genuine gate
            // crossing, e.g. an early devotion edge or a land-presence gate
            // flipping once) or Axis-1 escalations; they are NOT
            // under-escalation — `after` is always recomputed authoritatively
            // from the live board (CR 611.3a), so the short-circuit errs only
            // toward over-escalation, never stale derived state.
            //
            // Bound: `full_evals < steps/4 + 8` proves the near-O(1) collapse
            // (the measured 4 sits far under 38) while leaving headroom for the
            // handful of rules-mandatory flips. The per-axis synthetic dual tests
            // above pin the exact short-circuit / escalation decision per axis.
            assert!(
                full_evals < steps / 4 + 8,
                "truth-delta short-circuit must keep full evaluate_layers passes \
                 near-constant: got {full_evals} full passes across {steps} steps \
                 (stack was {stack_size}). Kruphix's `Not(DevotionGE 7)` gate is \
                 stable FALSE once devotion >= 7 (CR 700.5 / CR 611.3a), so \
                 devotion-perturbing copy-token entries must NOT escalate — a count \
                 anywhere near `steps` would mean the short-circuit regressed back \
                 to per-entry escalation."
            );
        }

        /// Build a battlefield with a Devotion-magnitude anthem source plus
        /// pre-existing creatures, then push a single token-creation trigger.
        /// Returns (state, anthem_source_id).
        ///
        /// The anthem is "creatures you control get +X/+X where X = your devotion
        /// to green" — a board-population-dependent magnitude
        /// (`DistinctColorsAmongPermanents`-class via `Devotion`). A token entry
        /// changes devotion, so the magnitude applied to PRE-EXISTING creatures
        /// must re-evaluate; the escalation scan must force a full pass.
        fn devotion_anthem_board() -> GameState {
            use crate::types::ability::DevotionColors;
            use crate::types::ability::{ContinuousModification, StaticDefinition};
            use crate::types::statics::StaticMode;
            let mut state = setup();
            // Two pre-existing green creatures.
            for i in 0..2 {
                let id = create_object(
                    &mut state,
                    CardId(50 + i),
                    PlayerId(0),
                    format!("Bear{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
            }
            // Anthem source: "creatures you control get +X/+X, X = devotion to green".
            let anthem = create_object(
                &mut state,
                CardId(60),
                PlayerId(0),
                "Devotion Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddDynamicPower {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Devotion {
                            colors: DevotionColors::Fixed(vec![ManaColor::Green]),
                        },
                    },
                },
                ContinuousModification::AddDynamicToughness {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Devotion {
                            colors: DevotionColors::Fixed(vec![ManaColor::Green]),
                        },
                    },
                },
            ];
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// (B1) Population-magnitude escalation: with a devotion-magnitude anthem,
        /// a token entry must escalate the incremental flush to a full pass so
        /// pre-existing creatures' P/T re-evaluate. Dual-run: the normal flush
        /// path must produce a board characteristic-identical to a forced-Full
        /// flush.
        #[test]
        fn devotion_anthem_token_entry_escalates_and_matches_full() {
            let mut base = devotion_anthem_board();
            let src = add_scute_source(&mut base);
            push_token_triggers(&mut base, src, insect_token_effect(), None, 1);

            // Normal path (incremental flush eligible; must escalate).
            let mut normal = base.clone();
            resolve_to_empty_batched(&mut normal);
            flush_layers(&mut normal);

            // Forced-full reference: same resolution, then force a full re-eval.
            let mut forced = base.clone();
            resolve_to_empty_batched(&mut forced);
            forced.layers_dirty = crate::types::game_state::LayersDirty::Full;
            evaluate_layers(&mut forced);

            assert_pt_identical(&normal, &forced, "devotion anthem escalation");
        }

        /// (B2) Recipient-local dynamic ("+1/+1 for each +1/+1 counter on IT",
        /// `CountersOn { Recipient }`) must NOT escalate — it does not read board
        /// population — and the incremental result still matches a full recompute.
        #[test]
        fn recipient_local_dynamic_does_not_escalate_and_matches_full() {
            use crate::types::ability::{ContinuousModification, ObjectScope, StaticDefinition};
            use crate::types::statics::StaticMode;
            let mut base = setup();
            // A creature with a recipient-local self-buff static and a +1/+1 counter.
            let id = create_object(
                &mut base,
                CardId(70),
                PlayerId(0),
                "Recipient Buff".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::SelfRef);
            sd.modifications = vec![ContinuousModification::AddDynamicPower {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: ObjectScope::Recipient,
                        counter_type: Some(CounterType::Plus1Plus1),
                    },
                },
            }];
            {
                let o = base.objects.get_mut(&id).unwrap();
                o.base_power = Some(1);
                o.base_toughness = Some(1);
                o.power = Some(1);
                o.toughness = Some(1);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.counters.insert(CounterType::Plus1Plus1, 2);
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
            }
            base.layers_dirty = crate::types::game_state::LayersDirty::Full;

            let src = add_scute_source(&mut base);
            push_token_triggers(&mut base, src, insect_token_effect(), None, 1);

            let mut normal = base.clone();
            // Prove the escalation predicate does NOT fire for this board: after
            // resolving, the dirty state right before flush should be EnteredObjects
            // and the incremental path must apply.
            FULL_EVALUATE_LAYERS_COUNT.store(0, Ordering::Relaxed);
            resolve_to_empty_batched(&mut normal);
            flush_layers(&mut normal);

            let mut forced = base.clone();
            resolve_to_empty_batched(&mut forced);
            forced.layers_dirty = crate::types::game_state::LayersDirty::Full;
            evaluate_layers(&mut forced);

            assert_pt_identical(&normal, &forced, "recipient-local dynamic");
        }

        /// (B-embedded) Population-dependent EMBEDDED THRESHOLD escalation.
        ///
        /// A continuous static whose AFFECTED FILTER is a `PtComparison` with an
        /// `ObjectCount`-backed threshold ("creatures with power <= the number of
        /// creatures you control get +1/+1"). A token entry changes the creature
        /// count, which changes the threshold, which changes whether PRE-EXISTING
        /// creatures match the affected filter. The escalation scan must fire via
        /// the `affected_filter_uses_object_population` → embedded-threshold
        /// `quantity_expr_uses_object_count` recursion, forcing a full pass.
        ///
        /// Dual-run: the normal flush path must produce a board characteristic-
        /// identical to a forced-Full flush.
        #[test]
        fn embedded_threshold_token_entry_escalates_and_matches_full() {
            use crate::types::ability::{
                Comparator, ContinuousModification, FilterProp, PtStat, PtValueScope,
                StaticDefinition,
            };
            use crate::types::statics::StaticMode;
            let mut base = setup();
            // Two pre-existing 1/1 green creatures.
            for i in 0..2 {
                let id = create_object(
                    &mut base,
                    CardId(80 + i),
                    PlayerId(0),
                    format!("Smol{i}"),
                    Zone::Battlefield,
                );
                let o = base.objects.get_mut(&id).unwrap();
                o.base_power = Some(1);
                o.base_toughness = Some(1);
                o.power = Some(1);
                o.toughness = Some(1);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
            }
            // Anthem source: "creatures you control with power <= (number of
            // creatures you control) get +1/+1" — affected set keyed by an
            // ObjectCount-backed PtComparison threshold.
            let anthem = create_object(
                &mut base,
                CardId(90),
                PlayerId(0),
                "Threshold Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                properties: vec![FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::LE,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter {
                                type_filters: vec![TypeFilter::Creature],
                                ..Default::default()
                            }),
                        },
                    },
                }],
                ..Default::default()
            }));
            sd.modifications = vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ];
            {
                let o = base.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            base.layers_dirty = crate::types::game_state::LayersDirty::Full;

            let src = add_scute_source(&mut base);
            push_token_triggers(&mut base, src, insect_token_effect(), None, 1);

            let mut normal = base.clone();
            resolve_to_empty_batched(&mut normal);
            flush_layers(&mut normal);

            let mut forced = base.clone();
            resolve_to_empty_batched(&mut forced);
            forced.layers_dirty = crate::types::game_state::LayersDirty::Full;
            evaluate_layers(&mut forced);

            assert_pt_identical(&normal, &forced, "embedded-threshold escalation");
        }

        /// (B-condition) Population-dependent source-level CONDITION escalation.
        ///
        /// A continuous anthem static "creatures you control get +1/+1 as long as
        /// you control 3 or more creatures" — a source-level enabling condition
        /// (`QuantityComparison` over `ObjectCount`) that gates the effect for the
        /// WHOLE recipient set (not recipient-local). The board starts one short
        /// of the threshold (2 creatures), so the condition is OFF and no creature
        /// is buffed. A single token entry crosses the threshold (→ 3 creatures),
        /// flipping the condition ON for EVERY pre-existing creature.
        ///
        /// The incremental flush re-derives only the entered token, so without the
        /// condition-axis escalation clause the pre-existing creatures would keep
        /// stale (unbuffed) P/T. The escalation scan must fire via
        /// `static_condition_uses_object_population` →
        /// `quantity_expr_uses_object_count`, forcing a full pass.
        ///
        /// Asserts (a) the entry escalated to a FULL pass (the full-eval counter
        /// incremented exactly once during the normal-path flush) and (b) dual-run
        /// characteristic-identity: the normal flush produces a board identical to
        /// a forced-Full flush, with pre-existing creatures at the flipped-on P/T.
        #[test]
        fn condition_gated_anthem_token_entry_escalates_and_matches_full() {
            use crate::types::ability::{
                Comparator, ContinuousModification, StaticCondition, StaticDefinition,
            };
            use crate::types::statics::StaticMode;
            let mut base = setup();
            // Two pre-existing 2/2 creatures — one short of the ≥3 threshold.
            let mut creature_ids = Vec::new();
            for i in 0..2 {
                let id = create_object(
                    &mut base,
                    CardId(100 + i),
                    PlayerId(0),
                    format!("Gater{i}"),
                    Zone::Battlefield,
                );
                let o = base.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                creature_ids.push(id);
            }
            // Anthem source (an Enchantment — does NOT count toward the creature
            // threshold): "creatures you control get +1/+1 as long as you control
            // 3 or more creatures".
            let anthem = create_object(
                &mut base,
                CardId(110),
                PlayerId(0),
                "Condition Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ];
            sd.condition = Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            ..Default::default()
                        }),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            });
            {
                let o = base.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            base.layers_dirty = crate::types::game_state::LayersDirty::Full;

            // Sanity: with 2 creatures the condition is OFF — no buff yet.
            flush_layers(&mut base);
            for &id in &creature_ids {
                let o = base.objects.get(&id).unwrap();
                assert_eq!(o.power, Some(2), "condition should be off below threshold");
            }

            let src = add_scute_source(&mut base);
            push_token_triggers(&mut base, src, insect_token_effect(), None, 1);

            // Normal path: incremental flush eligible; the condition-axis escalation
            // must force a full pass. Reset the counter BEFORE resolution so a
            // flush triggered inside the resolve pipeline (SBA / batch resolve)
            // is counted too — escalation must occur somewhere in the
            // resolve-then-flush window, not necessarily on the final explicit
            // flush (the pipeline may have already drained the EnteredObjects
            // mark by the time we flush below).
            let mut normal = base.clone();
            FULL_EVALUATE_LAYERS_COUNT.store(0, Ordering::Relaxed);
            resolve_to_empty_batched(&mut normal);
            flush_layers(&mut normal);
            let full_evals = FULL_EVALUATE_LAYERS_COUNT.load(Ordering::Relaxed);
            assert!(
                full_evals >= 1,
                "token entry crossing a board-population-gated condition must \
                 escalate the incremental flush to a full pass (got {full_evals})"
            );

            // Forced-full reference. Build AFTER reading the counter so its own
            // `evaluate_layers` does not perturb the measurement above.
            let mut forced = base.clone();
            resolve_to_empty_batched(&mut forced);
            forced.layers_dirty = crate::types::game_state::LayersDirty::Full;
            evaluate_layers(&mut forced);

            // Pre-existing creatures must be flipped ON (3/3), not stale (2/2).
            for &id in &creature_ids {
                let o = normal.objects.get(&id).unwrap();
                assert_eq!(
                    o.power,
                    Some(3),
                    "pre-existing creature must be buffed after the condition flips on"
                );
            }
            assert_pt_identical(&normal, &forced, "condition-gated anthem escalation");
        }

        // ====================================================================
        // ENTRY-AWARE escalation tests (cheap-reject classifier + entry-
        // membership narrowing). Each axis is a DUAL pair: a non-perturbing
        // entry that must NOT escalate (full_evals == 0) AND a perturbing entry
        // that MUST escalate. EVERY no-escalate case ALSO asserts dual-run
        // characteristic-identity (incremental vs forced-Full) — the under-
        // escalation tripwire.
        // ====================================================================

        /// ISOLATED single-flush measurement of the entry-aware escalation
        /// decision. `setup_board` returns a board with the anthem already in
        /// place (still `Full`-dirty). The helper:
        ///   1. flushes the board to Clean (the anthem's initial full pass — NOT
        ///      measured),
        ///   2. invokes `add_entry` to create the entering object and returns its
        ///      id (the closure must `mark_layers_entered` so the dirty lattice is
        ///      `EnteredObjects`),
        ///   3. resets the counter and performs a SINGLE `flush_layers`, capturing
        ///      exactly the entry-aware escalation decision (0 = incremental fast
        ///      path engaged, >=1 = escalated to a full pass),
        ///   4. builds a forced-Full reference from the same post-entry board for
        ///      dual-run characteristic identity.
        ///
        /// This isolates the escalation DECISION from the token-RESOLUTION
        /// pipeline (which does unrelated full passes during `Effect::Token`
        /// resolution / SBA).
        ///
        /// The escalation signal is read RACE-FREE from
        /// `incremental_flush_must_escalate` directly (a pure predicate over the
        /// post-entry board) rather than from the process-wide
        /// `FULL_EVALUATE_LAYERS_COUNT`, which `cargo test`'s parallel runner
        /// would otherwise corrupt. Returns `(incremental_board, escalated,
        /// forced_full_board)`, where `escalated == false` means the entry-aware
        /// fast path engaged and `escalated == true` means the entry forced a full
        /// pass. The dual-run identity (`incremental_board` vs `forced_full_board`)
        /// is the under-escalation tripwire regardless of the decision.
        fn flush_entry_and_forced(
            setup_board: impl Fn() -> GameState,
            add_entry: impl Fn(&mut GameState) -> ObjectId,
        ) -> (GameState, bool, GameState) {
            // Normal path: flush the anthem in, add the entry, read the decision,
            // then flush incrementally (or full, per the decision).
            let mut normal = setup_board();
            flush_layers(&mut normal);
            add_entry(&mut normal);
            let entered_ids: std::collections::HashSet<ObjectId> = match &normal.layers_dirty {
                crate::types::game_state::LayersDirty::EnteredObjects(ids) => ids.clone(),
                other => panic!("expected EnteredObjects dirty state, got {other:?}"),
            };
            let escalated =
                crate::game::layers::incremental_flush_must_escalate(&normal, &entered_ids);
            flush_layers(&mut normal);

            // Forced-Full reference: same board + entry, then a full re-eval.
            let mut forced = setup_board();
            flush_layers(&mut forced);
            add_entry(&mut forced);
            forced.layers_dirty = crate::types::game_state::LayersDirty::Full;
            evaluate_layers(&mut forced);
            (normal, escalated, forced)
        }

        /// Create a plain colorless, non-land creature ("Insect"-like) entry and
        /// mark layers entered. Flips no devotion / land-presence gate and matches
        /// no artifact/land filter.
        fn add_colorless_creature_entry(state: &mut GameState, card_id: u64) -> ObjectId {
            let id = create_object(
                state,
                CardId(card_id),
                PlayerId(0),
                "Insect".to_string(),
                Zone::Battlefield,
            );
            {
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(1);
                o.base_toughness = Some(1);
                o.power = Some(1);
                o.toughness = Some(1);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_color = vec![];
                o.color = vec![];
            }
            crate::game::layers::mark_layers_entered(state, id);
            id
        }

        /// (1a) Devotion gate — NON-perturbing: a colorless creature entering under
        /// a devotion-to-green magnitude anthem flips no green devotion symbol, so
        /// the entry must stay on the incremental path (full_evals==0) AND the
        /// incremental board must match a forced-Full board.
        #[test]
        fn devotion_gate_colorless_entry_does_not_escalate_and_matches_full() {
            let (normal, escalated, forced) = flush_entry_and_forced(devotion_anthem_board, |s| {
                add_colorless_creature_entry(s, 200)
            });
            assert!(
                !escalated,
                "colorless entry flips no green devotion shard — must not escalate"
            );
            assert_pt_identical(&normal, &forced, "devotion gate colorless non-escalation");
        }

        /// (1b) Devotion gate — PERTURBING: a green {G}-cost permanent entering DOES
        /// add a green devotion symbol (CR 700.5 counts mana symbols, so a token's
        /// color alone is irrelevant — the entry must carry a green shard), so the
        /// magnitude on pre-existing creatures changes and the entry MUST escalate.
        #[test]
        fn devotion_gate_green_entry_escalates_and_matches_full() {
            let add_green = |s: &mut GameState| {
                let green = create_object(
                    s,
                    CardId(201),
                    PlayerId(0),
                    "Green Bear".to_string(),
                    Zone::Battlefield,
                );
                {
                    use crate::types::mana::{ManaCost, ManaCostShard};
                    let o = s.objects.get_mut(&green).unwrap();
                    o.base_card_types.core_types = vec![CoreType::Creature];
                    o.card_types.core_types = vec![CoreType::Creature];
                    o.base_color = vec![ManaColor::Green];
                    o.color = vec![ManaColor::Green];
                    o.mana_cost = ManaCost::Cost {
                        shards: vec![ManaCostShard::Green],
                        generic: 0,
                    };
                    o.base_mana_cost = o.mana_cost.clone();
                }
                crate::game::layers::mark_layers_entered(s, green);
                green
            };
            let (normal, escalated, forced) =
                flush_entry_and_forced(devotion_anthem_board, add_green);
            assert!(
                escalated,
                "green {{G}}-cost permanent entry moves devotion — must escalate"
            );
            assert_pt_identical(&normal, &forced, "devotion gate green escalation");
        }

        /// Build a board with an `IsPresent(Land)`-gated anthem: "creatures you
        /// control get +1/+1 as long as you control a land". Two pre-existing
        /// creatures, no land yet (gate OFF).
        fn is_present_land_board() -> GameState {
            use crate::types::ability::{
                ContinuousModification, StaticCondition, StaticDefinition,
            };
            use crate::types::statics::StaticMode;
            let mut state = setup();
            for i in 0..2 {
                let id = create_object(
                    &mut state,
                    CardId(210 + i),
                    PlayerId(0),
                    format!("LandGater{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
            }
            let anthem = create_object(
                &mut state,
                CardId(220),
                PlayerId(0),
                "Land Presence Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ];
            sd.condition = Some(StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Land))),
            });
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// (2a) IsPresent(Land) gate — NON-perturbing: a colorless creature entry
        /// does not satisfy the Land filter, so the land-presence gate cannot
        /// flip; must NOT escalate AND must match a forced-Full board.
        #[test]
        fn is_present_land_creature_entry_does_not_escalate_and_matches_full() {
            let (normal, escalated, forced) = flush_entry_and_forced(is_present_land_board, |s| {
                add_colorless_creature_entry(s, 231)
            });
            assert!(
                !escalated,
                "creature entry doesn't match Land filter — gate can't flip, no escalation"
            );
            assert_pt_identical(&normal, &forced, "IsPresent(Land) creature non-escalation");
        }

        /// (2b) IsPresent(Land) gate — PERTURBING: a land entering satisfies the
        /// Land filter and flips the gate from OFF to ON for every pre-existing
        /// creature; MUST escalate AND match a forced-Full board.
        #[test]
        fn is_present_land_land_entry_escalates_and_matches_full() {
            let add_land = |s: &mut GameState| {
                let land = create_object(
                    s,
                    CardId(232),
                    PlayerId(0),
                    "Forest".to_string(),
                    Zone::Battlefield,
                );
                {
                    let o = s.objects.get_mut(&land).unwrap();
                    o.base_card_types.core_types = vec![CoreType::Land];
                    o.card_types.core_types = vec![CoreType::Land];
                }
                crate::game::layers::mark_layers_entered(s, land);
                land
            };
            let (normal, escalated, forced) =
                flush_entry_and_forced(is_present_land_board, add_land);
            assert!(
                escalated,
                "land entry flips IsPresent(Land) ON — must escalate"
            );
            assert_pt_identical(&normal, &forced, "IsPresent(Land) land escalation");
        }

        /// Build a board with a count-anthem magnitude keyed by "artifacts you
        /// control": "creatures you control get +X/+X, X = number of artifacts
        /// you control". Two pre-existing creatures.
        fn artifact_count_anthem_board() -> GameState {
            use crate::types::ability::{
                ContinuousModification, StaticDefinition, TypeFilter as TF, TypedFilter as TFil,
            };
            use crate::types::statics::StaticMode;
            let mut state = setup();
            for i in 0..2 {
                let id = create_object(
                    &mut state,
                    CardId(240 + i),
                    PlayerId(0),
                    format!("CountBear{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
            }
            let anthem = create_object(
                &mut state,
                CardId(250),
                PlayerId(0),
                "Artifact Count Anthem".to_string(),
                Zone::Battlefield,
            );
            let artifact_filter = TargetFilter::Typed(TFil::new(TF::Artifact));
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TFil::new(TF::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddDynamicPower {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: artifact_filter.clone(),
                        },
                    },
                },
                ContinuousModification::AddDynamicToughness {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: artifact_filter,
                        },
                    },
                },
            ];
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// Create a colorless artifact (non-land, non-creature) entry and mark
        /// layers entered.
        fn add_artifact_entry(state: &mut GameState, card_id: u64) -> ObjectId {
            let id = create_object(
                state,
                CardId(card_id),
                PlayerId(0),
                "Treasure".to_string(),
                Zone::Battlefield,
            );
            {
                let o = state.objects.get_mut(&id).unwrap();
                o.base_card_types.core_types = vec![CoreType::Artifact];
                o.card_types.core_types = vec![CoreType::Artifact];
            }
            crate::game::layers::mark_layers_entered(state, id);
            id
        }

        /// (3a) Count-anthem (ObjectCount artifacts) — NON-perturbing: a colorless
        /// creature entry doesn't match "artifacts you control", so the magnitude
        /// on pre-existing creatures cannot change; must NOT escalate AND match
        /// full.
        #[test]
        fn count_anthem_nonmatching_entry_does_not_escalate_and_matches_full() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(artifact_count_anthem_board, |s| {
                    add_colorless_creature_entry(s, 251)
                });
            assert!(
                !escalated,
                "creature entry doesn't match artifact count filter — no escalation"
            );
            assert_pt_identical(&normal, &forced, "count-anthem non-matching non-escalation");
        }

        /// (3b) Count-anthem (ObjectCount artifacts) — PERTURBING: an artifact
        /// entry matches the count filter, changing the magnitude applied to
        /// pre-existing creatures; MUST escalate AND match full.
        #[test]
        fn count_anthem_matching_entry_escalates_and_matches_full() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(artifact_count_anthem_board, |s| add_artifact_entry(s, 252));
            assert!(
                escalated,
                "artifact entry matches artifact count filter — must escalate"
            );
            assert_pt_identical(&normal, &forced, "count-anthem matching escalation");
        }

        /// (4) MEDIUM-2 — whole-board TALLY affected filter
        /// (`MostPrevalentCreatureTypeIn`). The anthem affects "creatures of the
        /// most prevalent creature type on the battlefield". A creature token
        /// entry whose own type is NOT the anthem's inner concern can STILL flip
        /// which type is most prevalent for PRE-EXISTING creatures, so the entry
        /// MUST escalate UNCONDITIONALLY (independent of any entered-object filter
        /// match) AND match a forced-Full board.
        /// Build a board whose anthem affects "creatures of the most prevalent
        /// creature type on the battlefield" — a whole-board TALLY affected
        /// filter (`MostPrevalentCreatureTypeIn`). Two pre-existing Bears.
        fn most_prevalent_anthem_board() -> GameState {
            use crate::types::ability::{ContinuousModification, FilterProp, StaticDefinition};
            use crate::types::statics::StaticMode;
            let mut base = setup();
            for i in 0..2 {
                let id = create_object(
                    &mut base,
                    CardId(260 + i),
                    PlayerId(0),
                    format!("TallyBear{i}"),
                    Zone::Battlefield,
                );
                let o = base.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_card_types.subtypes = vec!["Bear".to_string()];
                o.card_types.subtypes = vec!["Bear".to_string()];
            }
            let anthem = create_object(
                &mut base,
                CardId(270),
                PlayerId(0),
                "Most Prevalent Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                properties: vec![FilterProp::MostPrevalentCreatureTypeIn {
                    zone: crate::types::zones::Zone::Battlefield,
                    scope: crate::types::ability::ControllerRef::You,
                }],
                ..Default::default()
            }));
            sd.modifications = vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ];
            {
                let o = base.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            base.layers_dirty = crate::types::game_state::LayersDirty::Full;
            base
        }

        #[test]
        fn most_prevalent_tally_entry_escalates_unconditionally_and_matches_full() {
            // The entered creature is an "Insect" — a DIFFERENT creature type than
            // the pre-existing Bears, so it does NOT match the anthem's current
            // "most prevalent" membership (Bear), yet adding it changes the tally
            // and so must escalate UNCONDITIONALLY (MEDIUM-2).
            let add_insect = |s: &mut GameState| {
                let id = create_object(
                    s,
                    CardId(271),
                    PlayerId(0),
                    "Insect".to_string(),
                    Zone::Battlefield,
                );
                {
                    let o = s.objects.get_mut(&id).unwrap();
                    o.base_power = Some(1);
                    o.base_toughness = Some(1);
                    o.power = Some(1);
                    o.toughness = Some(1);
                    o.base_card_types.core_types = vec![CoreType::Creature];
                    o.card_types.core_types = vec![CoreType::Creature];
                    o.base_card_types.subtypes = vec!["Insect".to_string()];
                    o.card_types.subtypes = vec!["Insect".to_string()];
                }
                crate::game::layers::mark_layers_entered(s, id);
                id
            };
            let (normal, escalated, forced) =
                flush_entry_and_forced(most_prevalent_anthem_board, add_insect);
            assert!(
                escalated,
                "whole-board tally (MostPrevalentCreatureTypeIn) must escalate \
                 unconditionally on ANY creature entry"
            );
            assert_pt_identical(
                &normal,
                &forced,
                "most-prevalent tally unconditional escalation",
            );
        }

        // ====================================================================
        // Truth-delta short-circuit tests (CR 611.3a + CR 611.3b).
        //
        // A source-level (non-recipient-context) population-gated CONTINUOUS
        // static no longer escalates an incremental flush merely because an
        // entry perturbs its gate INPUT — it escalates only when the gate TRUTH
        // flips. Recipient-context gates, magnitude perturbation (Axis 2a), and
        // key-absent fail-closed all still escalate unconditionally.
        // ====================================================================

        /// Build a board with a SOURCE-LEVEL `Not(DevotionGE {Green, 7})`-gated
        /// anthem ("creatures you control get +1/+1 as long as your devotion to
        /// green is LESS than 7"). The gate is whole-effect on/off (consumed at
        /// collection, `condition: None` on the active effect) and NON-recipient-
        /// context (`condition_uses_recipient_context` is false for `DevotionGE`,
        /// recursed through `Not`). `green_symbols` green mana symbols on the
        /// anthem source set the controller's baseline devotion (CR 700.5), so the
        /// caller controls whether a green {G} entry crosses the threshold-7 edge.
        /// Two pre-existing green creatures are the anthem recipients.
        fn devotion_gated_anthem_board(green_symbols: usize) -> GameState {
            use crate::types::ability::{
                ContinuousModification, StaticCondition, StaticDefinition,
            };
            use crate::types::mana::{ManaCost, ManaCostShard};
            use crate::types::statics::StaticMode;
            let mut state = setup();
            for i in 0..2 {
                let id = create_object(
                    &mut state,
                    CardId(300 + i),
                    PlayerId(0),
                    format!("DevBear{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
            }
            let anthem = create_object(
                &mut state,
                CardId(310),
                PlayerId(0),
                "Devotion-Gated Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ];
            // CR 700.5 + CR 611.3a: source-level gate "devotion to green < 7".
            sd.condition = Some(StaticCondition::Not {
                condition: Box::new(StaticCondition::DevotionGE {
                    colors: vec![ManaColor::Green],
                    threshold: 7,
                }),
            });
            let cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green; green_symbols],
                generic: 0,
            };
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
                o.mana_cost = cost.clone();
                o.base_mana_cost = cost;
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// Add a single green {G}-cost creature entry. Raises green devotion by
        /// exactly one mana symbol (CR 700.5).
        fn add_green_devotion_entry(state: &mut GameState, card_id: u64) -> ObjectId {
            use crate::types::mana::{ManaCost, ManaCostShard};
            let id = create_object(
                state,
                CardId(card_id),
                PlayerId(0),
                "Green Sprout".to_string(),
                Zone::Battlefield,
            );
            {
                let cost = ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                };
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(1);
                o.base_toughness = Some(1);
                o.power = Some(1);
                o.toughness = Some(1);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
                o.mana_cost = cost.clone();
                o.base_mana_cost = cost;
            }
            crate::game::layers::mark_layers_entered(state, id);
            id
        }

        /// (a) GATE-STAYS — the truth-delta short-circuit's discriminating case.
        /// Devotion is already 8 (>= 7), so `Not(DevotionGE 7)` is FALSE (gate
        /// OFF); a green {G} entry raises devotion to 9 — the gate INPUT is
        /// perturbed but its TRUTH stays FALSE. Under d9a40be71 the perturbation
        /// alone forced escalation; the truth-delta short-circuit must now skip
        /// it. `!escalated` FAILS under d9a40be71, PASSES after. `assert_pt_identical`
        /// confirms the incremental board (anthem off → base 2/2 recipients)
        /// matches a forced-full board.
        #[test]
        fn source_condition_gate_unchanged_does_not_escalate_and_matches_full() {
            let (normal, escalated, forced) = flush_entry_and_forced(
                || devotion_gated_anthem_board(8),
                |s| add_green_devotion_entry(s, 320),
            );
            assert!(
                !escalated,
                "green entry perturbs devotion but does not flip the < 7 gate \
                 (8 → 9, still >= 7) — truth-delta short-circuit must not escalate"
            );
            assert_pt_identical(&normal, &forced, "devotion gate unchanged non-escalation");
        }

        /// (b) GATE-FLIPS — baseline devotion 6 (< 7, gate ON, anthem applies
        /// +1/+1); a green {G} entry raises devotion to 7, flipping
        /// `Not(DevotionGE 7)` to FALSE (gate OFF). Every PRE-EXISTING recipient
        /// loses the buff, so the flush MUST escalate. `escalated` + match-full.
        #[test]
        fn source_condition_gate_flip_escalates_and_matches_full() {
            let (normal, escalated, forced) = flush_entry_and_forced(
                || devotion_gated_anthem_board(6),
                |s| add_green_devotion_entry(s, 321),
            );
            assert!(
                escalated,
                "green entry flips the < 7 gate (6 → 7) OFF — pre-existing \
                 recipients lose the anthem, must escalate"
            );
            assert_pt_identical(&normal, &forced, "devotion gate flip escalation");
        }

        /// Build a MULTI-AXIS anthem: BOTH a `Devotion`-backed magnitude (Axis 2a,
        /// population-sensitive) AND a source-level population-gated condition
        /// (`IsPresent(Creature)`, ON and stable). A green {G} entry perturbs the
        /// magnitude on PRE-EXISTING creatures, so Axis 2a must escalate FIRST —
        /// regardless of the condition's stable truth. Pins the multi-axis
        /// ordering (the truth-delta short-circuit must never suppress a magnitude
        /// perturbation).
        fn devotion_magnitude_and_condition_board() -> GameState {
            use crate::types::ability::{
                ContinuousModification, DevotionColors, StaticCondition, StaticDefinition,
            };
            use crate::types::statics::StaticMode;
            let mut state = setup();
            for i in 0..2 {
                let id = create_object(
                    &mut state,
                    CardId(330 + i),
                    PlayerId(0),
                    format!("MultiBear{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
            }
            let anthem = create_object(
                &mut state,
                CardId(340),
                PlayerId(0),
                "Multi-Axis Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddDynamicPower {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Devotion {
                            colors: DevotionColors::Fixed(vec![ManaColor::Green]),
                        },
                    },
                },
                ContinuousModification::AddDynamicToughness {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Devotion {
                            colors: DevotionColors::Fixed(vec![ManaColor::Green]),
                        },
                    },
                },
            ];
            // Source-level population gate, ON (creatures exist) and stable.
            sd.condition = Some(StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature))),
            });
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// (c) MULTI-AXIS — magnitude perturbation always escalates (Axis 2a),
        /// even though the source-level condition's truth is stable ON. A green
        /// {G} entry moves green devotion, changing the magnitude applied to
        /// PRE-EXISTING creatures; the truth-delta short-circuit must NOT
        /// suppress this. `escalated` + match-full.
        #[test]
        fn source_condition_and_magnitude_always_escalates() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(devotion_magnitude_and_condition_board, |s| {
                    add_green_devotion_entry(s, 341)
                });
            assert!(
                escalated,
                "magnitude (devotion) perturbation must escalate via Axis 2a \
                 regardless of the stable source-level condition truth"
            );
            assert_pt_identical(&normal, &forced, "multi-axis magnitude escalation");
        }

        /// Build a RECIPIENT-CONTEXT population-gated anthem (the BLOCKER's
        /// discriminating guard). The condition is
        /// `QuantityComparison { ObjectCount { Creature AND Another } GE 3 }` —
        /// "as long as there are at least 3 OTHER creatures". `FilterProp::Another`
        /// makes the count recipient-relative (`filter_uses_recipient` true), so
        /// the gate is RE-EVALUATED PER RECIPIENT (`evaluate_condition_with_recipient`
        /// threads `recipient` into the count, excluding that recipient) and
        /// `source_condition_gate_passes` only OVER-approximates it. It is also
        /// population-sensitive (`ObjectCount`). With 3 pre-existing creatures,
        /// each recipient sees 2 OTHERS (gate OFF). A 4th creature entry makes
        /// each PRE-EXISTING recipient see 3 others → its per-recipient gate flips
        /// ON. A single board-level boolean cannot summarize this, so a
        /// recipient-context gate must ALWAYS escalate (never short-circuit).
        /// Ships green-and-stale WITHOUT the recipient-context exclusion.
        fn recipient_context_count_anthem_board() -> GameState {
            use crate::types::ability::{
                Comparator, ContinuousModification, FilterProp, StaticCondition, StaticDefinition,
            };
            use crate::types::statics::StaticMode;
            let mut state = setup();
            // Three pre-existing creatures (recipients of the anthem).
            for i in 0..3 {
                let id = create_object(
                    &mut state,
                    CardId(350 + i),
                    PlayerId(0),
                    format!("CountBear{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
            }
            let anthem = create_object(
                &mut state,
                CardId(360),
                PlayerId(0),
                "Other-Creatures Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ];
            // Recipient-relative count: "creatures other than the recipient".
            let other_creatures = TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                properties: vec![FilterProp::Another],
                ..Default::default()
            });
            sd.condition = Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: other_creatures,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            });
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// (d) RECIPIENT-CONTEXT (BLOCKER guard) — a population-gated condition
        /// whose truth is PER-RECIPIENT must always escalate when perturbed, even
        /// though `source_condition_gate_passes` would report a single, possibly-
        /// unchanged board-level value. A 4th creature flips each pre-existing
        /// recipient's "at least 3 other creatures" gate ON, so escalation is
        /// mandatory. `escalated` + match-full. This ships green-and-stale WITHOUT
        /// the recipient-context exclusion (the discriminating BLOCKER guard).
        #[test]
        fn recipient_context_population_condition_always_escalates_and_matches_full() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(recipient_context_count_anthem_board, |s| {
                    add_colorless_creature_entry(s, 361)
                });
            assert!(
                escalated,
                "recipient-context population gate re-evaluates per recipient — \
                 a threshold-edge creature entry flips pre-existing recipients' \
                 gates; must escalate unconditionally (never short-circuit)"
            );
            assert_pt_identical(
                &normal,
                &forced,
                "recipient-context unconditional escalation",
            );
        }

        /// (e) FAIL-CLOSED KEY-ABSENT — when a source-level population-gated
        /// static's key is ABSENT from `static_gate_truth` (e.g. the cache was
        /// never refreshed for it, or it was phased out at the last full eval),
        /// the consult must FAIL CLOSED and escalate. Prime the board, perturb,
        /// then clear the cache before consulting `incremental_flush_must_escalate`
        /// directly — the missing BEFORE truth forces a conservative full pass.
        #[test]
        fn absent_gate_key_escalates() {
            let mut state = devotion_gated_anthem_board(6);
            flush_layers(&mut state);
            // A green entry perturbs the < 7 gate (would flip 6 → 7).
            add_green_devotion_entry(&mut state, 322);
            let entered_ids: std::collections::HashSet<ObjectId> = match &state.layers_dirty {
                crate::types::game_state::LayersDirty::EnteredObjects(ids) => ids.clone(),
                other => panic!("expected EnteredObjects, got {other:?}"),
            };
            // Simulate a stale/absent cache: drop every recorded gate truth.
            state.static_gate_truth.clear();
            assert!(
                crate::game::layers::incremental_flush_must_escalate(&state, &entered_ids),
                "absent gate-truth key must fail closed and escalate (invariant 1)"
            );
        }

        /// Assert every battlefield object's computed power/toughness/loyalty and
        /// keyword set are identical across two states.
        fn assert_pt_identical(a: &GameState, b: &GameState, label: &str) {
            assert_eq!(
                a.battlefield.len(),
                b.battlefield.len(),
                "{label}: battlefield size mismatch"
            );
            for &id in a.battlefield.iter() {
                let oa = a.objects.get(&id).expect("a object");
                let ob = b.objects.get(&id).expect("b object");
                assert_eq!(oa.power, ob.power, "{label}: power mismatch for {id:?}");
                assert_eq!(
                    oa.toughness, ob.toughness,
                    "{label}: toughness mismatch for {id:?}"
                );
                assert_eq!(
                    oa.keywords, ob.keywords,
                    "{label}: keyword mismatch for {id:?}"
                );
            }
        }
    }

    /// CR 706.2 + CR 706.4 + CR 603.12: A reflexive "When you do … the result"
    /// sub-ability resolves on its OWN `StackEntryKind::TriggeredAbility` entry,
    /// in a later resolution scope than the original roll. The rolled value is
    /// carried on the entry's `die_result` field and re-stamped into
    /// `die_result_this_resolution` by `resolve_top` so the entry's
    /// `EventContextAmount` reads the roll (11), NOT the surviving combat-damage
    /// event amount (6). This is the building-block guard for Ancient Bronze
    /// Dragon's reflexive class (issue #1602, Deliverable 1).
    #[test]
    fn reflexive_entry_lifts_carried_die_result_into_resolution_scope() {
        let mut state = setup();
        // A source object on the battlefield (controller P0).
        let source = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Ancient Bronze Dragon".to_string(),
            Zone::Battlefield,
        );

        // The reflexive sub-ability: "gain life equal to the result".
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::EventContextAmount,
                },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        );

        // Carry die_result: Some(11) onto the entry, alongside a SURVIVING
        // combat-damage trigger event (amount 6). Match-count is None so the
        // die slot is what the cascade must read.
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: source,
            target: TargetRef::Player(PlayerId(0)),
            amount: 6,
            is_combat: true,
            excess: 0,
        });
        state.stack.push_back(StackEntry {
            id: source,
            source_id: source,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: source,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: "Ancient Bronze Dragon".to_string(),
                subject_match_count: None,
                die_result: Some(11),
            },
        });

        let life_before = state.players[0].life;
        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Gained 11 (the carried die result), NOT 6 (the combat-damage event).
        assert_eq!(
            state.players[0].life - life_before,
            11,
            "reflexive entry must read the carried die result (11), not the \
             surviving combat-damage amount (6)"
        );
        // The die slot is cleared at the cross-resolution boundary after the
        // entry resolves (mirrors the batched subject-count lifecycle).
        assert_eq!(state.die_result_this_resolution, None);
        assert_eq!(state.current_trigger_match_count, None);
    }
}
