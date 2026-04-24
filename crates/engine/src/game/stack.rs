use crate::types::ability::{
    CastingPermission, ContinuousModification, Duration, EffectKind, KeywordAction,
    ResolvedAbility, TargetFilter,
};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{CastingVariant, GameState, StackEntry, StackEntryKind};
use crate::types::identifiers::ObjectId;
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

/// CR 608.2: Resolve the top object on the stack.
pub fn resolve_top(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // CR 405.5: When all players pass in succession, the top object on the stack resolves.
    let entry = match state.stack.pop_back() {
        Some(e) => e,
        None => return,
    };

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
    }

    // Extract the resolved ability from the stack entry. `KeywordAction` is
    // handled by the early return above and never reaches this match.
    let (ability, is_spell, casting_variant, actual_mana_spent) = match &entry.kind {
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

    // Capture targets for Aura attachment after resolution
    let spell_targets = ability
        .as_ref()
        .map(|a| a.targets.clone())
        .unwrap_or_default();

    // Only run targeting validation and effect execution when an ability exists.
    // Permanent spells with no spell ability (ability is None) skip straight to
    // zone-change handling below.
    if let Some(ref ability) = ability {
        let original_targets = flatten_targets_in_chain(ability);
        if !original_targets.is_empty() {
            let validated = validate_targets_in_chain(state, ability);
            let legal_targets = flatten_targets_in_chain(&validated);
            if targeting::check_fizzle(&original_targets, &legal_targets) {
                // CR 608.2b: Fizzle — all targets illegal, spell is countered on resolution.
                if is_spell {
                    // CR 702.34a / CR 702.180a: Flashback and Harmonize exile when leaving
                    // the stack for any reason, including fizzle. Escape is included for consistency.
                    let dest = if matches!(
                        casting_variant,
                        CastingVariant::Flashback
                            | CastingVariant::Harmonize
                            | CastingVariant::Escape
                    ) {
                        Zone::Exile
                    } else {
                        Zone::Graveyard
                    };
                    zones::move_to_zone(state, entry.id, dest, events);
                }
                events.push(GameEvent::StackResolved {
                    object_id: entry.id,
                });
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
        } else if casting_variant == CastingVariant::Harmonize {
            // CR 702.180a: If the harmonize cost was paid, exile this card instead of putting it anywhere else.
            if is_permanent_type(state, entry.id) {
                Zone::Battlefield
            } else {
                Zone::Exile
            }
        } else if casting_variant == CastingVariant::Flashback {
            // CR 702.34a: If the flashback cost was paid, exile this card
            // instead of putting it anywhere else any time it would leave the stack.
            // Flashback only appears on instants/sorceries — unconditional exile is correct.
            Zone::Exile
        } else if is_permanent_type(state, entry.id) {
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
                    // CR 603.4: Propagate cast_from_zone to the permanent so ETB triggers
                    // can evaluate conditions like "if you cast it from your hand".
                    // When ability is present, use its context; otherwise the object
                    // already has cast_from_zone set during finalize_cast_to_stack.
                    if let Some(ref ability) = ability {
                        if let Some(obj) = state.objects.get_mut(&entry.id) {
                            obj.cast_from_zone = ability.context.cast_from_zone;
                        }
                    }
                    // CR 614.12a: Drain mandatory replacement post-effects (e.g., the
                    // Siege protector / Tribute opponent-choice prompt that was stashed
                    // by `apply_single_replacement` while resolving this ZoneChange).
                    // Sets `state.waiting_for` to the resulting prompt, if any — the
                    // caller's post-stack resolution checks waiting_for before returning
                    // priority. Without this drain the choice would be silently dropped.
                    if let Some(effect_def) = state.post_replacement_effect.take() {
                        let _ = super::engine_replacement::apply_post_replacement_effect(
                            state,
                            &effect_def,
                            Some(entry.id),
                            None,
                            events,
                        );
                    }
                }
                super::replacement::ReplacementResult::Prevented => {
                    // CR 608.3e: Permanent spell's ETB was fully prevented —
                    // the card goes to owner's graveyard instead.
                    zones::move_to_zone(state, entry.id, Zone::Graveyard, events);
                }
                super::replacement::ReplacementResult::NeedsChoice(player) => {
                    // A replacement needs player choice (e.g., Clone "enter as a copy").
                    // Store context so handle_replacement_choice can complete post-resolution.
                    let cast_from_zone = ability
                        .as_ref()
                        .and_then(|a| a.context.cast_from_zone)
                        .or_else(|| state.objects.get(&entry.id).and_then(|o| o.cast_from_zone));
                    state.pending_spell_resolution =
                        Some(crate::types::game_state::PendingSpellResolution {
                            object_id: entry.id,
                            controller: entry.controller,
                            casting_variant,
                            cast_from_zone,
                            spell_targets: spell_targets.clone(),
                            actual_mana_spent,
                        });
                    state.waiting_for =
                        super::replacement::replacement_choice_waiting_for(player, state);
                    // Emit StackResolved now — the spell has left the stack even though
                    // the replacement choice is pending.
                    events.push(GameEvent::StackResolved {
                        object_id: entry.id,
                    });
                    state.current_trigger_event = None;
                    return;
                }
            }
        } else {
            zones::move_to_zone(state, entry.id, dest, events);
        }

        // CR 715.3d: When an Adventure spell resolves to exile, restore the creature face
        // and grant AdventureCreature permission so it can be cast from exile.
        if casting_variant == CastingVariant::Adventure {
            if let Some(obj) = state.objects.get_mut(&entry.id) {
                // Restore creature face characteristics (swap back from Adventure face)
                if let Some(creature_face) = obj.back_face.take() {
                    let adventure_snapshot = super::printed_cards::snapshot_object_face(obj);
                    super::printed_cards::apply_back_face_to_object(obj, creature_face);
                    obj.back_face = Some(adventure_snapshot);
                }
                obj.casting_permissions
                    .push(crate::types::ability::CastingPermission::AdventureCreature);
            }
        }

        // CR 303.4f: Aura resolving to battlefield attaches to its target.
        if dest == Zone::Battlefield {
            let is_aura = state
                .objects
                .get(&entry.id)
                .map(|obj| obj.card_types.subtypes.iter().any(|s| s == "Aura"))
                .unwrap_or(false);
            if is_aura {
                if let Some(crate::types::ability::TargetRef::Object(target_id)) =
                    spell_targets.first()
                {
                    // Verify target is still on the battlefield
                    if state.battlefield.contains(target_id) {
                        effects::attach::attach_to(state, entry.id, *target_id);
                    }
                    // If target is gone, SBA check_unattached_auras will handle cleanup
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
                effects::attach::attach_to(state, equipment_id, target_creature_id);
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
            let still_on_battlefield = state
                .objects
                .get(&spacecraft_id)
                .is_some_and(|sc| sc.zone == Zone::Battlefield);
            if still_on_battlefield && counters_added > 0 {
                effects::counters::add_counter_with_replacement(
                    state,
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
    let mut last_key: Option<(
        String,
        &'static str,
        Option<String>,
        Vec<crate::types::ability::TargetRef>,
    )> = None;
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
        let (name, tag, desc, targets) = group_key(state, entry);
        let owned_key = (name, tag, desc.map(str::to_owned), targets.to_vec());
        if last_key.as_ref() == Some(&owned_key) {
            let last = out.last_mut().unwrap();
            last.count += 1;
            last.member_ids.push(entry.id);
        } else {
            out.push(StackDisplayGroup {
                representative: entry.id,
                count: 1,
                member_ids: vec![entry.id],
            });
            last_key = Some(owned_key);
        }
    }
    out
}

/// Grouping signature for `stack_display_groups`. Two entries coalesce iff
/// their signatures are equal. Includes the resolved target vector so
/// visually-identical triggers that fire against different targets (e.g.
/// N copies of "target player loses 1 life" picking different players)
/// remain separate — coalescing them would misrepresent the resolution.
fn group_key<'a>(
    state: &'a GameState,
    entry: &'a StackEntry,
) -> (
    String,
    &'static str,
    Option<&'a str>,
    &'a [crate::types::ability::TargetRef],
) {
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
    let targets: &[crate::types::ability::TargetRef] =
        entry.ability().map(|a| a.targets.as_slice()).unwrap_or(&[]);
    (source_name, tag, description, targets)
}

/// CR 110.4: Permanent types that resolve to the battlefield.
fn is_permanent_type(state: &GameState, object_id: ObjectId) -> bool {
    use crate::types::card_type::CoreType;

    let obj = match state.objects.get(&object_id) {
        Some(o) => o,
        None => return false,
    };

    obj.card_types.core_types.iter().any(|ct| {
        matches!(
            ct,
            CoreType::Creature
                | CoreType::Artifact
                | CoreType::Enchantment
                | CoreType::Planeswalker
                | CoreType::Land
        )
    })
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
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
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
        Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, TypedFilter,
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
            object_id: ObjectId(999), // target doesn't matter for this test
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
            state.objects.get(&aura_id).unwrap().attached_to,
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
}
