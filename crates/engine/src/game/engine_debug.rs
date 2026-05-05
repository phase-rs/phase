use std::collections::HashSet;

use crate::types::actions::DebugAction;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{ActionResult, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

use super::effects::attach::attach_to;
use super::effects::change_zone::shuffle_library;
use super::engine::EngineError;
use super::game_object::AttachTarget;
use super::zones;

pub fn apply_debug_action(
    state: &mut GameState,
    _actor: PlayerId,
    action: DebugAction,
    events: &mut Vec<GameEvent>,
) -> Result<ActionResult, EngineError> {
    match action {
        DebugAction::MoveToZone {
            object_id,
            to_zone,
            simulate,
        } => {
            validate_object(state, object_id)?;
            zones::move_to_zone(state, object_id, to_zone, events);
            if simulate {
                super::sba::check_state_based_actions(state, events);
                super::triggers::process_triggers(state, events);
            }
            state.layers_dirty = true;
        }

        DebugAction::CreateCard { .. } => {
            return Err(EngineError::InvalidAction(
                "Debug::CreateCard must be handled at the WASM layer".into(),
            ));
        }

        DebugAction::RemoveObject { object_id } => {
            validate_object(state, object_id)?;
            let obj = &state.objects[&object_id];
            let zone = obj.zone;
            let owner = obj.owner;

            // Detach from target if attached
            if let Some(AttachTarget::Object(target_id)) = obj.attached_to {
                if let Some(target) = state.objects.get_mut(&target_id) {
                    target.attachments.retain(|&id| id != object_id);
                }
            }

            // Detach anything attached to this object
            let attachments: Vec<ObjectId> = state.objects[&object_id].attachments.clone();
            for att_id in attachments {
                if let Some(att) = state.objects.get_mut(&att_id) {
                    att.attached_to = None;
                }
            }

            zones::remove_from_zone(state, object_id, zone, owner);
            state.objects.remove(&object_id);
            state.layers_dirty = true;
        }

        DebugAction::DrawCards { player_id, count } => {
            validate_player(state, player_id)?;
            let proposed = ProposedEvent::Draw {
                player_id,
                count,
                applied: HashSet::new(),
            };
            match super::replacement::replace_event(state, proposed, events) {
                super::replacement::ReplacementResult::Execute(event) => {
                    super::effects::draw::apply_draw_after_replacement(state, event, events);
                }
                super::replacement::ReplacementResult::Prevented => {}
                super::replacement::ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for =
                        super::replacement::replacement_choice_waiting_for(player, state);
                }
            }
        }

        DebugAction::Mill { player_id, count } => {
            validate_player(state, player_id)?;
            let player = state.players.iter().find(|p| p.id == player_id).unwrap();
            let top_ids: Vec<ObjectId> = player
                .library
                .iter()
                .take(count as usize)
                .copied()
                .collect();
            for id in top_ids {
                zones::move_to_zone(state, id, Zone::Graveyard, events);
            }
        }

        DebugAction::ShuffleLibrary { player_id } => {
            validate_player(state, player_id)?;
            shuffle_library(state, player_id);
        }

        DebugAction::SetBasePowerToughness {
            object_id,
            power,
            toughness,
        } => {
            let obj = validate_object_mut(state, object_id)?;
            if let Some(p) = power {
                obj.base_power = Some(p);
            }
            if let Some(t) = toughness {
                obj.base_toughness = Some(t);
            }
            state.layers_dirty = true;
        }

        DebugAction::ModifyCounters {
            object_id,
            counter_type,
            delta,
        } => {
            let obj = validate_object_mut(state, object_id)?;
            if delta > 0 {
                *obj.counters.entry(counter_type.clone()).or_insert(0) += delta as u32;
            } else if delta < 0 {
                let entry = obj.counters.entry(counter_type.clone()).or_insert(0);
                *entry = entry.saturating_sub(delta.unsigned_abs());
                if *entry == 0 {
                    obj.counters.remove(&counter_type);
                }
            }
            // Sync derived fields with counter map
            if matches!(counter_type, CounterType::Loyalty) {
                let val = obj
                    .counters
                    .get(&CounterType::Loyalty)
                    .copied()
                    .unwrap_or(0);
                obj.loyalty = Some(val);
            }
            if matches!(counter_type, CounterType::Defense) {
                let val = obj
                    .counters
                    .get(&CounterType::Defense)
                    .copied()
                    .unwrap_or(0);
                obj.defense = Some(val);
            }
            if matches!(counter_type, CounterType::Lore) && obj.class_level.is_some() {
                let lore = obj.counters.get(&CounterType::Lore).copied().unwrap_or(0);
                obj.class_level = Some((lore as u8).max(1));
            }
            state.layers_dirty = true;
        }

        DebugAction::SetTapped { object_id, tapped } => {
            validate_object_mut(state, object_id)?.tapped = tapped;
        }

        DebugAction::SetController {
            object_id,
            controller,
        } => {
            validate_player(state, controller)?;
            validate_object_mut(state, object_id)?.controller = controller;
            state.layers_dirty = true;
        }

        DebugAction::SetSummoningSickness { object_id, sick } => {
            validate_object_mut(state, object_id)?.summoning_sick = sick;
        }

        DebugAction::SetFaceState {
            object_id,
            face_down,
            transformed,
            flipped,
        } => {
            let obj = validate_object_mut(state, object_id)?;
            if let Some(fd) = face_down {
                obj.face_down = fd;
            }
            if let Some(t) = transformed {
                obj.transformed = t;
            }
            if let Some(f) = flipped {
                obj.flipped = f;
            }
            state.layers_dirty = true;
        }

        DebugAction::Attach {
            object_id,
            target_id,
        } => {
            validate_object(state, object_id)?;
            validate_object(state, target_id)?;
            attach_to(state, object_id, target_id);
            state.layers_dirty = true;
        }

        DebugAction::Detach { object_id } => {
            validate_object(state, object_id)?;
            let attached_to = state.objects[&object_id].attached_to;
            if let Some(AttachTarget::Object(target_id)) = attached_to {
                if let Some(target) = state.objects.get_mut(&target_id) {
                    target.attachments.retain(|&id| id != object_id);
                }
            }
            if let Some(obj) = state.objects.get_mut(&object_id) {
                obj.attached_to = None;
            }
            state.layers_dirty = true;
        }

        DebugAction::GrantKeyword { object_id, keyword } => {
            let obj = validate_object_mut(state, object_id)?;
            if !obj.keywords.contains(&keyword) {
                obj.keywords.push(keyword);
            }
            state.layers_dirty = true;
        }

        DebugAction::RemoveKeyword { object_id, keyword } => {
            let obj = validate_object_mut(state, object_id)?;
            obj.keywords.retain(|k| k != &keyword);
            state.layers_dirty = true;
        }

        DebugAction::SetLife { player_id, life } => {
            validate_player(state, player_id)?;
            if let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) {
                player.life = life;
            }
        }

        DebugAction::AddMana { player_id, mana } => {
            validate_player(state, player_id)?;
            if let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) {
                for mana_type in mana {
                    player.mana_pool.add(crate::types::mana::ManaUnit::new(
                        mana_type,
                        ObjectId(0),
                        false,
                        vec![],
                    ));
                }
            }
        }

        DebugAction::SetPhase {
            phase,
            active_player,
        } => {
            validate_player(state, active_player)?;
            state.phase = phase;
            state.active_player = active_player;
            state.priority_player = active_player;
            state.combat = None;
            state.stack.clear();
            state.waiting_for = WaitingFor::Priority {
                player: active_player,
            };
        }

        DebugAction::RunStateBasedActions => {
            super::sba::check_state_based_actions(state, events);
            super::triggers::process_triggers(state, events);
        }

        DebugAction::CreateToken {
            owner,
            name,
            power,
            toughness,
            core_types,
            subtypes,
            colors,
            keywords,
        } => {
            validate_player(state, owner)?;
            let id = zones::create_object(
                state,
                crate::types::identifiers::CardId(0),
                owner,
                name,
                Zone::Battlefield,
            );
            if let Some(obj) = state.objects.get_mut(&id) {
                obj.is_token = true;
                obj.base_power = power;
                obj.base_toughness = toughness;
                obj.controller = owner;
                obj.card_types.core_types = core_types;
                obj.card_types.subtypes = subtypes;
                obj.color = colors;
                for kw in keywords {
                    if !obj.keywords.contains(&kw) {
                        obj.keywords.push(kw);
                    }
                }
                obj.summoning_sick = true;
            }
            state.layers_dirty = true;
        }
    }

    Ok(ActionResult {
        events: std::mem::take(events),
        waiting_for: state.waiting_for.clone(),
        log_entries: vec![],
    })
}

fn validate_object(state: &GameState, object_id: ObjectId) -> Result<(), EngineError> {
    if !state.objects.contains_key(&object_id) {
        return Err(EngineError::InvalidAction(format!(
            "Debug: object {} not found",
            object_id.0
        )));
    }
    Ok(())
}

fn validate_object_mut(
    state: &mut GameState,
    object_id: ObjectId,
) -> Result<&mut crate::game::game_object::GameObject, EngineError> {
    state.objects.get_mut(&object_id).ok_or_else(|| {
        EngineError::InvalidAction(format!("Debug: object {} not found", object_id.0))
    })
}

fn validate_player(state: &GameState, player_id: PlayerId) -> Result<(), EngineError> {
    if !state.players.iter().any(|p| p.id == player_id) {
        return Err(EngineError::InvalidAction(format!(
            "Debug: player {} not found",
            player_id.0
        )));
    }
    Ok(())
}
