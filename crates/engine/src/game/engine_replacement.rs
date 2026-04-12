use crate::ai_support::copy_target_mana_value_ceiling;
use crate::types::ability::{AbilityDefinition, Effect, ResolvedAbility, TargetFilter, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::effects;
use super::engine::EngineError;
use super::zones;

pub(super) fn handle_replacement_choice(
    state: &mut GameState,
    index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    match super::replacement::continue_replacement(state, index, events) {
        super::replacement::ReplacementResult::Execute(event) => {
            let mut zone_change_object_id = None;
            let mut enters_battlefield = false;
            if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                object_id,
                to,
                from,
                enter_tapped,
                enter_with_counters,
                controller_override,
                enter_transformed,
                ..
            } = event
            {
                zones::move_to_zone(state, object_id, to, events);
                // CR 400.7: reset_for_battlefield_entry (inside move_to_zone) sets
                // defaults. Override only when the replacement pipeline changed them.
                if to == Zone::Battlefield {
                    if let Some(obj) = state.objects.get_mut(&object_id) {
                        if enter_tapped {
                            obj.tapped = true;
                        }
                        if let Some(new_controller) = controller_override {
                            obj.controller = new_controller;
                        }
                        // CR 614.1c: Apply counters from replacement pipeline.
                        apply_etb_counters(obj, &enter_with_counters, events);
                        // CR 614.1c: Apply pending ETB counters from delayed triggers
                        // (e.g., "that creature enters with an additional +1/+1 counter").
                        let pending: Vec<_> = state
                            .pending_etb_counters
                            .iter()
                            .filter(|(oid, _, _)| *oid == object_id)
                            .map(|(_, ct, n)| (ct.clone(), *n))
                            .collect();
                        if !pending.is_empty() {
                            apply_etb_counters(obj, &pending, events);
                            state
                                .pending_etb_counters
                                .retain(|(oid, _, _)| *oid != object_id);
                        }
                    }
                }
                // CR 712.14a: Apply transformation if entering the battlefield transformed.
                if enter_transformed && to == Zone::Battlefield {
                    if let Some(obj) = state.objects.get(&object_id) {
                        if obj.back_face.is_some() && !obj.transformed {
                            let _ = crate::game::transform::transform_permanent(
                                state, object_id, events,
                            );
                        }
                    }
                }
                if to == Zone::Battlefield || from == Zone::Battlefield {
                    state.layers_dirty = true;
                }
                enters_battlefield = to == Zone::Battlefield;
                zone_change_object_id = Some(object_id);
            }

            let mut waiting_for = WaitingFor::Priority {
                player: state.active_player,
            };
            state.waiting_for = waiting_for.clone();

            let mut replacement_ctx = None;
            if let Some(ctx) = state.pending_spell_resolution.take() {
                if enters_battlefield {
                    apply_pending_spell_resolution(state, &ctx);
                }
                replacement_ctx = Some(ctx);
            }

            if let Some(effect_def) = state.post_replacement_effect.take() {
                if let Some(next_waiting_for) = apply_post_replacement_effect(
                    state,
                    &effect_def,
                    zone_change_object_id,
                    replacement_ctx.as_ref(),
                    events,
                ) {
                    waiting_for = next_waiting_for;
                }
            }

            if matches!(waiting_for, WaitingFor::Priority { .. }) {
                if let Some(cont) = state.pending_continuation.take() {
                    let _ = effects::resolve_ability_chain(state, &cont, events, 0);
                }
            }

            Ok(waiting_for)
        }
        super::replacement::ReplacementResult::NeedsChoice(player) => Ok(
            super::replacement::replacement_choice_waiting_for(player, state),
        ),
        super::replacement::ReplacementResult::Prevented => {
            // CR 608.3e: If the ETB was prevented during spell resolution,
            // the permanent goes to the graveyard instead.
            if let Some(ctx) = state.pending_spell_resolution.take() {
                zones::move_to_zone(state, ctx.object_id, Zone::Graveyard, events);
            }
            state.pending_continuation = None;
            Ok(WaitingFor::Priority {
                player: state.active_player,
            })
        }
    }
}

pub(super) fn handle_copy_target_choice(
    state: &mut GameState,
    waiting_for: WaitingFor,
    target: Option<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let WaitingFor::CopyTargetChoice {
        player,
        source_id,
        valid_targets,
        ..
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for copy target choice".to_string(),
        ));
    };

    let target_id = match target {
        Some(TargetRef::Object(id)) if valid_targets.contains(&id) => id,
        _ => {
            return Err(EngineError::InvalidAction(
                "Invalid copy target".to_string(),
            ))
        }
    };

    let ability = copy_effect_for_source(state, source_id)
        .map(|effect_def| {
            resolved_ability_from_definition(
                effect_def,
                source_id,
                player,
                vec![TargetRef::Object(target_id)],
            )
        })
        .unwrap_or_else(|| {
            ResolvedAbility::new(
                Effect::BecomeCopy {
                    target: TargetFilter::Any,
                    duration: None,
                    mana_value_limit: None,
                    additional_modifications: Vec::new(),
                },
                vec![TargetRef::Object(target_id)],
                source_id,
                player,
            )
        });
    let _ = effects::resolve_ability_chain(state, &ability, events, 0);
    state.layers_dirty = true;
    if let Some(cont) = state.pending_continuation.take() {
        let _ = effects::resolve_ability_chain(state, &cont, events, 0);
    }
    Ok(WaitingFor::Priority {
        player: state.active_player,
    })
}

fn copy_effect_for_source(state: &GameState, source_id: ObjectId) -> Option<&AbilityDefinition> {
    state
        .objects
        .get(&source_id)?
        .replacement_definitions
        .iter()
        .filter_map(|replacement| replacement.execute.as_deref())
        .find(|effect_def| matches!(&*effect_def.effect, Effect::BecomeCopy { .. }))
}

/// Apply a post-replacement side effect after a zone change has been executed.
/// Used by Optional replacements (e.g., shock lands: pay life on accept, tap on decline).
/// CR 707.9: For "enter as a copy" replacements, sets up CopyTargetChoice instead of
/// immediate resolution, since the player must choose which permanent to copy.
pub(super) fn apply_post_replacement_effect(
    state: &mut GameState,
    effect_def: &AbilityDefinition,
    object_id: Option<ObjectId>,
    spell_resolution: Option<&crate::types::game_state::PendingSpellResolution>,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let (source_id, controller) = object_id
        .and_then(|obj_id| {
            state
                .objects
                .get(&obj_id)
                .map(|obj| (obj_id, obj.controller))
        })
        .unwrap_or((ObjectId(0), state.active_player));

    if let Effect::BecomeCopy { ref target, .. } = *effect_def.effect {
        let max_mana_value = spell_resolution
            .and_then(|ctx| copy_target_mana_value_ceiling(ctx.actual_mana_spent, effect_def));
        let valid_targets = find_copy_targets(state, target, source_id, controller, max_mana_value);
        if valid_targets.is_empty() {
            return None;
        }
        return Some(WaitingFor::CopyTargetChoice {
            player: controller,
            source_id,
            valid_targets,
            max_mana_value,
        });
    }

    let targets = object_id
        .map(TargetRef::Object)
        .into_iter()
        .collect::<Vec<_>>();
    let resolved = resolved_ability_from_definition(effect_def, source_id, controller, targets);
    let _ = effects::resolve_ability_chain(state, &resolved, events, 0);

    match &state.waiting_for {
        WaitingFor::Priority { .. } => None,
        wf => Some(wf.clone()),
    }
}

/// CR 608.3: Complete post-resolution work for a permanent spell whose ETB
/// went through the replacement pipeline and required a player choice.
/// Applies cast_from_zone, aura attachment, and warp delayed triggers.
fn apply_pending_spell_resolution(
    state: &mut GameState,
    ctx: &crate::types::game_state::PendingSpellResolution,
) {
    use crate::types::game_state::CastingVariant;

    // CR 603.4: Propagate cast_from_zone so ETB triggers can evaluate
    // conditions like "if you cast it from your hand".
    if let Some(obj) = state.objects.get_mut(&ctx.object_id) {
        obj.cast_from_zone = ctx.cast_from_zone;
    }

    // CR 303.4f: Aura resolving to battlefield attaches to its target.
    let is_aura = state
        .objects
        .get(&ctx.object_id)
        .map(|obj| obj.card_types.subtypes.iter().any(|s| s == "Aura"))
        .unwrap_or(false);
    if is_aura {
        if let Some(crate::types::ability::TargetRef::Object(target_id)) = ctx.spell_targets.first()
        {
            if state.battlefield.contains(target_id) {
                effects::attach::attach_to(state, ctx.object_id, *target_id);
            }
        }
    }

    // CR 702.185a: Warp delayed trigger setup.
    if ctx.casting_variant == CastingVariant::Warp {
        let has_warp = state.objects.get(&ctx.object_id).is_some_and(|obj| {
            obj.keywords
                .iter()
                .any(|k| matches!(k, crate::types::keywords::Keyword::Warp(_)))
        });
        if has_warp {
            super::stack::create_warp_delayed_trigger(state, ctx.object_id, ctx.controller);
        }
    }
}

pub(super) fn apply_etb_counters(
    obj: &mut super::game_object::GameObject,
    counters: &[(String, u32)],
    events: &mut Vec<GameEvent>,
) {
    for (counter_type_str, count) in counters {
        let ct = crate::types::counter::parse_counter_type(counter_type_str);
        *obj.counters.entry(ct.clone()).or_insert(0) += count;
        events.push(GameEvent::CounterAdded {
            object_id: obj.id,
            counter_type: ct,
            count: *count,
        });
    }
}

fn find_copy_targets(
    state: &GameState,
    filter: &TargetFilter,
    source_id: ObjectId,
    controller: PlayerId,
    max_mana_value: Option<u32>,
) -> Vec<ObjectId> {
    let ctx = super::filter::FilterContext::from_source_with_controller(source_id, controller);
    state
        .objects
        .iter()
        .filter(|(id, obj)| {
            obj.zone == Zone::Battlefield
                && **id != source_id
                && max_mana_value.is_none_or(|max| obj.mana_cost.mana_value() <= max)
                && super::filter::matches_target_filter(state, **id, filter, &ctx)
        })
        .map(|(id, _)| *id)
        .collect()
}

fn resolved_ability_from_definition(
    def: &AbilityDefinition,
    source_id: ObjectId,
    controller: PlayerId,
    targets: Vec<TargetRef>,
) -> ResolvedAbility {
    let mut resolved =
        ResolvedAbility::new(*def.effect.clone(), targets, source_id, controller).kind(def.kind);
    if let Some(sub) = &def.sub_ability {
        resolved = resolved.sub_ability(resolved_ability_from_definition(
            sub,
            source_id,
            controller,
            Vec::new(),
        ));
    }
    if let Some(else_ab) = &def.else_ability {
        resolved.else_ability = Some(Box::new(resolved_ability_from_definition(
            else_ab,
            source_id,
            controller,
            Vec::new(),
        )));
    }
    if let Some(d) = def.duration.clone() {
        resolved = resolved.duration(d);
    }
    if let Some(c) = def.condition.clone() {
        resolved = resolved.condition(c);
    }
    resolved
}
