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
            if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                object_id,
                to,
                from,
                enter_tapped,
                enter_with_counters,
                controller_override,
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
                if to == Zone::Battlefield || from == Zone::Battlefield {
                    state.layers_dirty = true;
                }
                zone_change_object_id = Some(object_id);
            }

            let mut waiting_for = WaitingFor::Priority {
                player: state.active_player,
            };
            state.waiting_for = waiting_for.clone();

            if let Some(effect_def) = state.post_replacement_effect.take() {
                if let Some(next_waiting_for) =
                    apply_post_replacement_effect(state, &effect_def, zone_change_object_id, events)
                {
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

    let ability = ResolvedAbility::new(
        Effect::BecomeCopy {
            target: TargetFilter::Any,
            duration: None,
        },
        vec![TargetRef::Object(target_id)],
        source_id,
        player,
    );
    let _ = effects::resolve_ability_chain(state, &ability, events, 0);
    state.layers_dirty = true;
    if let Some(cont) = state.pending_continuation.take() {
        let _ = effects::resolve_ability_chain(state, &cont, events, 0);
    }
    Ok(WaitingFor::Priority {
        player: state.active_player,
    })
}

/// Apply a post-replacement side effect after a zone change has been executed.
/// Used by Optional replacements (e.g., shock lands: pay life on accept, tap on decline).
/// CR 707.9: For "enter as a copy" replacements, sets up CopyTargetChoice instead of
/// immediate resolution, since the player must choose which permanent to copy.
pub(super) fn apply_post_replacement_effect(
    state: &mut GameState,
    effect_def: &AbilityDefinition,
    object_id: Option<ObjectId>,
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
        let valid_targets = find_copy_targets(state, target, source_id, controller);
        if valid_targets.is_empty() {
            return None;
        }
        return Some(WaitingFor::CopyTargetChoice {
            player: controller,
            source_id,
            valid_targets,
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
) -> Vec<ObjectId> {
    state
        .objects
        .iter()
        .filter(|(id, obj)| {
            obj.zone == Zone::Battlefield
                && **id != source_id
                && super::filter::matches_target_filter_controlled(
                    state, **id, filter, source_id, controller,
                )
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
