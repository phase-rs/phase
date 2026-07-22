use std::collections::HashSet;

use crate::types::ability::{
    Effect, LibraryPosition, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::actions::{DebugAction, DebugTokenRequest};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{ActionResult, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::{PlayerCounterKind, PlayerId};
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

use super::effects::attach::{attach_to, attach_to_player};
use super::effects::change_zone::shuffle_library;
use super::engine::EngineError;
use super::game_object::AttachTarget;
use super::zones;
use crate::game::token_presets::TokenPtProvenance;

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
            library_position,
            simulate,
        } => {
            validate_object(state, object_id)?;
            // Debug forces a zone change — route through the zone pipeline under
            // the `DebugCommand` exempt cause, which is FULLY inert: it skips
            // both the replacement consult and the delivery tail (no
            // enters-with-counter statics, no pending-ETB-counter consumption,
            // no devour snapshot), while the unconditional primitive guards
            // still run. DebugCommand is non-pausing by construction (always
            // `Done`), so the result is safely discarded. The library-position
            // arm folds the raw `move_to_library_position` / `_at_index`
            // siblings in via the placement request.
            let mut req = crate::game::zone_pipeline::ZoneMoveRequest::debug(object_id, to_zone);
            if to_zone == Zone::Library {
                req = req.at_library_position(library_position.unwrap_or(LibraryPosition::Bottom));
            }
            crate::game::zone_pipeline::move_object(state, req, events);
            if simulate {
                super::sba::check_state_based_actions(state, events);
                super::triggers::process_triggers(state, events);
            }
            crate::game::layers::mark_layers_full(state);
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

            // allow-raw-zone: debug-only object deletion forces state, not a CR zone-change event (CR 400.1).
            zones::remove_from_zone(state, object_id, zone, owner);
            state.objects.remove(&object_id);
            crate::game::layers::mark_layers_full(state);
        }

        DebugAction::Sacrifice { object_id } => {
            validate_object(state, object_id)?;
            // CR 701.21: A player sacrifices a permanent they control. Route
            // through the single sacrifice authority so the replacement pipeline
            // (e.g. Rest in Peace → exile) and dies/leaves-the-battlefield
            // triggers fire — unlike `RemoveObject`, which deletes the object
            // outright with no triggers.
            let controller = state.objects[&object_id].controller;
            match super::sacrifice::sacrifice_permanent(state, object_id, controller, events)
                .map_err(|err| EngineError::InvalidAction(format!("{err:?}")))?
            {
                super::sacrifice::SacrificeOutcome::Complete => {
                    super::triggers::process_triggers(state, events); // CR 603: dies/LTB triggers
                    let delayed = super::triggers::check_delayed_triggers(state, events);
                    events.extend(delayed);
                    super::sba::check_state_based_actions(state, events); // CR 704
                }
                super::sacrifice::SacrificeOutcome::NeedsReplacementChoice(player) => {
                    state.waiting_for =
                        super::replacement::replacement_choice_waiting_for(player, state);
                }
            }
        }

        DebugAction::DrawCards { player_id, count } => {
            validate_player(state, player_id)?;
            // CR 121.6b + CR 614.6 + CR 614.11 + CR 704.3: route through
            // `resume_multi_draw` (not the raw `draw_through_replacement`) so a
            // `count > 1` debug draw offers replacement independently per unit,
            // matching the real draw pipeline, and post-replacement
            // continuations (Jace WinTheGame, Abundance reveal-until) still
            // drain in the same step.
            let event_start = events.len();
            let result = super::effects::draw::start_draw_sequence(state, player_id, count, events);
            // CR 603.2: Mirror the normal draw pipeline — `PassPriority` /
            // `run_post_action_pipeline` scans CardDrawn events after the draw
            // step's turn-based action. Debug draw previously returned without
            // that scan, so draw triggers (Sheoldred, Rhystic Study, etc.) never
            // fired unless a replacement-choice round-trip happened to run the
            // pipeline. Defer trigger/SBA processing while a replacement choice
            // is open; the choice handler owns the post-draw scan.
            if !matches!(
                result,
                super::replacement::ReplacementResult::NeedsChoice(_)
            ) {
                let draw_events: Vec<_> = events[event_start..].to_vec();
                super::triggers::process_triggers(state, &draw_events);
                super::sba::check_state_based_actions(state, events);
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
            // Debug mill — route through the pipeline under `DebugCommand`
            // (fully inert: no consult, no delivery tail; non-pausing by
            // construction, so the result is safely discarded).
            for id in top_ids {
                let req = crate::game::zone_pipeline::ZoneMoveRequest::debug(id, Zone::Graveyard);
                crate::game::zone_pipeline::move_object(state, req, events);
            }
        }

        DebugAction::Reveal { player_id, count } => {
            validate_player(state, player_id)?;
            // CR 701.20a/b: Reveal the top `count` cards of the player's library
            // via the real `Effect::RevealTop` resolver — marks them revealed and
            // emits `CardsRevealed` without moving the cards. `TargetFilter::Any`
            // + an explicit `TargetRef::Player` makes the resolver reveal exactly
            // the requested library (see `reveal_top::resolve`).
            let ability = ResolvedAbility::new(
                Effect::RevealTop {
                    player: TargetFilter::Any,
                    count,
                },
                vec![TargetRef::Player(player_id)],
                ObjectId(0),
                player_id,
            );
            super::effects::reveal_top::resolve(state, &ability, events)
                .map_err(|err| EngineError::InvalidAction(format!("{err:?}")))?;
        }

        DebugAction::ShuffleLibrary { player_id } => {
            validate_player(state, player_id)?;
            shuffle_library(state, player_id, events);
        }

        DebugAction::Proliferate { player_id } => {
            validate_player(state, player_id)?;
            let ability = ResolvedAbility::new(Effect::Proliferate, vec![], ObjectId(0), player_id);
            super::effects::proliferate::resolve(state, &ability, events)
                .map_err(|err| EngineError::InvalidAction(format!("{err:?}")))?;
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
            crate::game::layers::mark_layers_full(state);
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
            crate::game::layers::mark_layers_full(state);
        }

        DebugAction::SetTapped { object_id, tapped } => {
            validate_object_mut(state, object_id)?.tapped = tapped;
        }

        DebugAction::SetPrepared {
            object_id,
            prepared,
        } => {
            // CR 722.3a/b: Route through the single authority so the
            // prepare-face gate and Became(Un)Prepared events are honored
            // instead of writing `obj.prepared` directly.
            validate_object_mut(state, object_id)?;
            if prepared {
                super::effects::prepare::prepare_object(state, object_id, events);
            } else {
                super::effects::prepare::unprepare_object(state, object_id, events);
            }
        }

        DebugAction::SetController {
            object_id,
            controller,
        } => {
            validate_player(state, controller)?;
            let obj = validate_object_mut(state, object_id)?;
            // CR 110.2 + CR 613.1b: A permanent's controller is a Layer-2
            // derived property. `evaluate_layers` Step 1 resets `obj.controller`
            // to `base_controller` on every pass, so a debug controller change
            // must write the base — the Layer-2 input — exactly as
            // `SetBasePowerToughness` writes base P/T and
            // `apply_battlefield_entry_controller_override` writes both fields.
            obj.base_controller = Some(controller);
            obj.controller = controller;
            crate::game::layers::mark_layers_full(state);
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
            validate_object(state, object_id)?;
            if let Some(fd) = face_down {
                validate_object_mut(state, object_id)?.face_down = fd;
            }
            if let Some(f) = flipped {
                validate_object_mut(state, object_id)?.flipped = f;
            }
            if let Some(want_transformed) = transformed {
                let (zone, has_back_face, currently_transformed) = {
                    let obj = state.objects.get(&object_id).unwrap();
                    (obj.zone, obj.back_face.is_some(), obj.transformed)
                };
                if want_transformed != currently_transformed {
                    // CR 701.27a: toggling `transformed` on a DFC must swap
                    // printed faces, not just flip the flag — a flag-only write
                    // leaves zone-exit revert applying the wrong characteristics
                    // (issue #3290 / debug transform tool, issue #3284).
                    if zone == Zone::Battlefield && has_back_face {
                        crate::game::transform::transform_permanent(state, object_id, events)?;
                    } else {
                        validate_object_mut(state, object_id)?.transformed = want_transformed;
                    }
                }
            }
            crate::game::layers::mark_layers_full(state);
        }

        DebugAction::Attach { object_id, target } => {
            validate_object(state, object_id)?;
            match target {
                AttachTarget::Object(target_id) => {
                    validate_object(state, target_id)?;
                    attach_to(state, object_id, target_id);
                }
                AttachTarget::Player(target_player) => {
                    validate_player(state, target_player)?;
                    attach_to_player(state, object_id, target_player);
                }
            }
            crate::game::layers::mark_layers_full(state);
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
            crate::game::layers::mark_layers_full(state);
        }

        DebugAction::GrantKeyword { object_id, keyword } => {
            let obj = validate_object_mut(state, object_id)?;
            // CR 613.1 + CR 613.1f: keywords are a Layer-6 derived property;
            // `evaluate_layers` resets `obj.keywords` to `base_keywords` on every
            // pass, so a debug grant must write the base — the Layer-6 input — or
            // the very next `layers_dirty` recompute wipes it. Same pattern as
            // `SetBasePowerToughness` (base P/T) and `SetController` (base controller).
            if !obj.base_keywords.contains(&keyword) {
                obj.base_keywords.push(keyword);
            }
            crate::game::layers::mark_layers_full(state);
        }

        DebugAction::RemoveKeyword { object_id, keyword } => {
            let obj = validate_object_mut(state, object_id)?;
            // CR 613.1 + CR 613.1f: write the base keyword set (the Layer-6 input)
            // so the removal survives the layer recompute; see GrantKeyword above.
            obj.base_keywords.retain(|k| k != &keyword);
            crate::game::layers::mark_layers_full(state);
        }

        DebugAction::SetLife { player_id, life } => {
            validate_player(state, player_id)?;
            if let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) {
                player.life = life;
            }
        }

        DebugAction::ModifyPlayerCounters {
            player_id,
            counter_kind,
            delta,
        } => {
            validate_player(state, player_id)?;
            apply_player_counter_delta(state, player_id, counter_kind, delta, events);
        }

        DebugAction::ModifyEnergy { player_id, delta } => {
            validate_player(state, player_id)?;
            apply_energy_delta(state, player_id, delta, events);
        }

        DebugAction::AddMana { player_id, mana } => {
            validate_player(state, player_id)?;
            for mana_type in mana {
                // CR 118.3a: route through the stamping authority so each
                // debug-added unit gets a distinct `pip_id`, exactly like
                // produced mana. A bare `mana_pool.add` leaves the unstamped
                // sentinel (`ManaPipId(0)`) on every unit, which makes all of
                // them pin/unpin together in the manual-payment UI.
                let _ = state.add_mana_to_pool(
                    player_id,
                    crate::types::mana::ManaUnit::new(mana_type, ObjectId(0), false, vec![]),
                );
            }
        }

        DebugAction::SetInfiniteMana { player_id, enabled } => {
            validate_player(state, player_id)?;
            if enabled {
                // Delegate to the single write authority; record the six Mana axes.
                state.mark_unbounded_loop(player_id, &super::mana_payment::INFINITE_MANA_AXES);
                // CR 500.5 debug exemption marker: tag this player's Mana axes as the debug
                // toggle so the end-of-step keep-gate suppresses the empty for them only (a
                // loop-backed Mana axis, absent from this set, drains and de-realizes instead).
                state.debug_infinite_mana.insert(player_id);
                // Seed immediately so the pool reads full before the next probe.
                super::mana_payment::refill_infinite_mana(state);
            } else {
                state.debug_infinite_mana.remove(&player_id);
                state.clear_unbounded_loop(player_id);
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

        DebugAction::CreateToken { request, run_etb } => {
            let (owner, characteristics, enter_with_counters, preset_image_ref) = match request {
                DebugTokenRequest::Preset {
                    preset_id,
                    owner,
                    power_override,
                    toughness_override,
                    enter_with_counters,
                } => {
                    let preset = crate::game::token_presets::known_token_preset_by_id(&preset_id)
                        .ok_or_else(|| {
                        EngineError::InvalidAction(format!(
                            "Debug: unknown token preset id {preset_id}"
                        ))
                    })?;
                    let mut characteristics = preset.body.clone();
                    match (&preset.pt_provenance, power_override, toughness_override) {
                        (
                            TokenPtProvenance::SourceDefinedOrDynamic { .. },
                            Some(power),
                            Some(toughness),
                        ) => {
                            characteristics.power = Some(power);
                            characteristics.toughness = Some(toughness);
                        }
                        (TokenPtProvenance::SourceDefinedOrDynamic { .. }, _, _) => {
                            return Err(EngineError::InvalidAction(format!(
                                "Debug: token preset {preset_id} requires both power_override and toughness_override"
                            )));
                        }
                        (TokenPtProvenance::FixedOrAbsent, None, None) => {}
                        (TokenPtProvenance::FixedOrAbsent, _, _) => {
                            return Err(EngineError::InvalidAction(format!(
                                "Debug: token preset {preset_id} has fixed or absent P/T and does not accept overrides"
                            )));
                        }
                    }
                    (
                        owner,
                        characteristics,
                        enter_with_counters,
                        preset.token_image_ref.clone(),
                    )
                }
                DebugTokenRequest::Custom {
                    owner,
                    characteristics,
                    enter_with_counters,
                } => (owner, characteristics, enter_with_counters, None),
            };
            validate_player(state, owner)?;
            // CR 111.1 + CR 614.1a: Route debug token creation through the real
            // CreateToken pipeline so replacements, predefined-subtype
            // abilities (Treasure/Clue/Food/etc.), and ETB triggers all fire.
            // CR 122.6a: `enter_with_counters` is plumbed straight to
            // `TokenSpec` and travels the same replacement pipeline as
            // engine-driven token creation — debug spawns can give bodies the
            // counters they need to survive SBA without bypassing CR 614.
            let spec = crate::types::proposed_event::TokenSpec {
                script_name: characteristics.display_name.clone(),
                characteristics,
                static_abilities: Vec::new(),
                enter_with_counters,
                tapped: false,
                enters_attacking: false,
                sacrifice_at: None,
                source_id: ObjectId(0),
                controller: owner,
                attach_to: None,
            };
            let proposed = ProposedEvent::CreateToken {
                owner,
                spec: Box::new(spec),
                copy: None,
                enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
                count: 1,
                applied: HashSet::new(),
            };
            match super::replacement::replace_event(state, proposed, events) {
                super::replacement::ReplacementResult::Execute(event) => {
                    super::effects::token::apply_create_token_after_replacement(
                        state, event, events,
                    );
                    // CR 111.4 + CR 707.2a: Preset spawns must install catalog
                    // `rules_text` abilities (SOS Pest attack-life trigger, etc.)
                    // after linking the preset image ref. The apply path runs
                    // `inject_catalog_token_abilities` during creation when
                    // `token_image_ref` is already set; debug preset creation
                    // deferred the ref until here, so inject + reindex now.
                    if let Some(image_ref) = preset_image_ref {
                        let created_ids = state.last_created_token_ids.clone();
                        for token_id in created_ids {
                            if let Some(obj) = state.objects.get_mut(&token_id) {
                                obj.token_image_ref = Some(image_ref.clone());
                            }
                            super::effects::token::inject_catalog_token_abilities(state, token_id);
                            super::trigger_index::reindex_object_triggers(state, token_id);
                        }
                    }
                    // "Run ETB effects" unchecked: the token is still created
                    // (with its replacement-window counters) but its ETB triggers
                    // and the SBA pass are skipped — mirrors the raw placement of
                    // `MoveToZone { simulate: false }`.
                    if run_etb {
                        super::triggers::process_triggers(state, events); // CR 603: Process triggers
                        super::sba::check_state_based_actions(state, events); // CR 704: Check SBAs
                    }
                }
                super::replacement::ReplacementResult::Prevented => {}
                super::replacement::ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for =
                        super::replacement::replacement_choice_waiting_for(player, state);
                }
            }
        }

        DebugAction::CreateTokenCopy { source_id, owner } => {
            validate_object(state, source_id)?;
            validate_player(state, owner)?;
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
                owner,
            );
            super::effects::token_copy::resolve(state, &ability, events)
                .map_err(|err| EngineError::InvalidAction(format!("{err:?}")))?;
            super::triggers::process_triggers(state, events);
            super::sba::check_state_based_actions(state, events);
        }
    }

    // CR 508.1a / CR 509.1a: A debug mutation can change attacker/blocker
    // eligibility (summoning sickness, tapped status, Haste/Defender) while the
    // engine is paused mid-declare-step. Re-derive the declare-step eligibility
    // snapshot so the refreshed payload is captured by the `ActionResult` below.
    // A genuine no-op for all non-declaration waiting states.
    super::combat::refresh_combat_declaration_waiting_for(state);

    Ok(ActionResult {
        events: std::mem::take(events),
        waiting_for: state.waiting_for.clone(),
        log_entries: vec![],
    })
}

fn apply_player_counter_delta(
    state: &mut GameState,
    player_id: PlayerId,
    counter_kind: PlayerCounterKind,
    delta: i32,
    events: &mut Vec<GameEvent>,
) {
    let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) else {
        return;
    };
    let before = player.player_counter(&counter_kind);
    if delta > 0 {
        player.add_player_counters(&counter_kind, delta as u32);
    } else if delta < 0 {
        player.remove_player_counters(&counter_kind, delta.unsigned_abs());
    }
    let after = player.player_counter(&counter_kind);
    let actual_delta = after as i32 - before as i32;
    if actual_delta != 0 {
        events.push(GameEvent::PlayerCounterChanged {
            player: player_id,
            counter_kind,
            delta: actual_delta,
        });
    }
}

fn apply_energy_delta(
    state: &mut GameState,
    player_id: PlayerId,
    delta: i32,
    events: &mut Vec<GameEvent>,
) {
    let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) else {
        return;
    };
    let before = player.energy;
    if delta > 0 {
        player.energy += delta as u32;
    } else if delta < 0 {
        player.energy = player.energy.saturating_sub(delta.unsigned_abs());
    }
    let after = player.energy;
    let actual_delta = after as i32 - before as i32;
    if actual_delta != 0 {
        events.push(GameEvent::EnergyChanged {
            player: player_id,
            delta: actual_delta,
        });
    }
}

/// CR 400.7 + CR 614.1: Route a debug-created object through the standard
/// battlefield-entry pipeline (replacements → move-to-zone → ETB triggers →
/// SBAs). Caller must have already created the object in an off-battlefield
/// staging zone (typically `Zone::Hand`) with face data applied. Returns the
/// resulting events and any new `WaitingFor` (e.g. replacement choice).
///
/// CR 303.4f: For Auras / Equipment, the caller is expected to wire
/// `attached_to` through `attach_to` / `attach_to_player` BEFORE invoking
/// this function. When that happens, the post-ETB SBA pass (CR 704.5n) sees
/// the attachment with a legal host and leaves it on the battlefield;
/// otherwise SBA correctly moves the orphan to its owner's graveyard. Both
/// behaviors are valid debug spawn paths — the choice belongs at the
/// caller (the WASM `handle_debug_create_card` bridge).
pub fn route_debug_create_to_battlefield(
    state: &mut GameState,
    object_id: ObjectId,
    run_etb: bool,
) -> ActionResult {
    use super::replacement::{self, ReplacementResult};

    let mut events: Vec<GameEvent> = vec![];

    // "Run ETB effects" unchecked: place the staged object on the battlefield
    // raw — no replacement window, no ETB triggers, no SBA pass. This mirrors
    // `MoveToZone { simulate: false }`, letting a board position be staged
    // without the entering permanent's "when ~ enters" abilities going on the
    // stack.
    if !run_etb {
        // Debug staging — route through the pipeline under `DebugCommand`,
        // which is FULLY inert: no replacement consult AND no delivery tail
        // (no intrinsic or statics-derived enters-with counters, no
        // pending-ETB-counter consumption, no devour snapshot), matching the
        // prior raw placement exactly. ETB triggers / SBA are NOT run here;
        // that is `run_etb`'s job below. DebugCommand is non-pausing by
        // construction (always `Done`), so the result is safely discarded.
        let req = crate::game::zone_pipeline::ZoneMoveRequest::debug(object_id, Zone::Battlefield);
        crate::game::zone_pipeline::move_object(state, req, &mut events);
        crate::game::layers::mark_layers_full(state);
        return ActionResult {
            events,
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        };
    }

    let from = state
        .objects
        .get(&object_id)
        .map(|o| o.zone)
        .unwrap_or(Zone::Hand);

    let proposed = ProposedEvent::ZoneChange {
        object_id,
        from,
        to: Zone::Battlefield,
        cause: None,
        attach_to: None,
        enter_tapped: Default::default(),
        enter_with_counters: vec![],
        controller_override: None,
        enter_transformed: false,
        face_down_profile: None,
        enter_as_copy: None,
        applied: HashSet::new(),
    };

    let mut waiting_for = state.waiting_for.clone();
    match replacement::replace_event(state, proposed, &mut events) {
        ReplacementResult::Execute(event) => {
            // CR 614.12a: a Devour as-enters sacrifice may surface its own
            // `EffectZoneChoice`; park on it so the debug-place flow keeps the
            // pending sacrifice prompt instead of overwriting it.
            match super::effects::change_zone::deliver_replaced_zone_change(
                state,
                event,
                None,
                None,
                false,
                crate::types::game_state::PostReplacementDrainOwner::DeliveryTail,
                None,
                &mut events,
            ) {
                super::effects::change_zone::ZoneDeliveryResult::Done => {}
                super::effects::change_zone::ZoneDeliveryResult::NeedsChoice(player) => {
                    replacement::park_waiting_for(state, player);
                    waiting_for = state.waiting_for.clone();
                }
            }
            super::triggers::process_triggers(state, &events); // CR 603: Process triggers
            super::sba::check_state_based_actions(state, &mut events); // CR 704: Check SBAs
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(player) => {
            waiting_for = replacement::replacement_choice_waiting_for(player, state);
        }
    }

    ActionResult {
        events,
        waiting_for,
        log_entries: vec![],
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::BackFaceData;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityDefinition, AbilityKind};
    use crate::types::actions::GameAction;
    use crate::types::card::LayoutKind;
    use crate::types::definitions::Definitions;
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost};
    use crate::types::proposed_event::TokenCharacteristics;
    use crate::types::CoreType;

    fn sandbox_state() -> GameState {
        let mut state = GameState::new(FormatConfig::standard().with_sandbox(), 2, 42);
        state.debug_mode = true;
        state
    }

    /// CR 118.3a regression: debug-added mana must route through the stamping
    /// authority so each unit gets a DISTINCT, nonzero `pip_id`. A bare
    /// `mana_pool.add` leaves every unit at the unstamped sentinel (0), which
    /// makes all same-color pips in the manual-payment UI pin/unpin together.
    #[test]
    fn debug_add_mana_stamps_distinct_pip_ids() {
        let mut state = sandbox_state();
        let mut events = Vec::new();
        apply_debug_action(
            &mut state,
            PlayerId(0),
            DebugAction::AddMana {
                player_id: PlayerId(0),
                mana: vec![
                    crate::types::mana::ManaType::Green,
                    crate::types::mana::ManaType::Green,
                    crate::types::mana::ManaType::Green,
                ],
            },
            &mut events,
        )
        .unwrap();

        let ids: Vec<u64> = state.players[0]
            .mana_pool
            .mana
            .iter()
            .map(|u| u.pip_id.0)
            .collect();
        assert_eq!(ids.len(), 3, "three AddMana entries → three pool units");
        assert!(
            ids.iter().all(|&id| id != 0),
            "debug-added units must be stamped (nonzero pip_id), got {ids:?}"
        );
        assert_eq!(
            ids.iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3,
            "debug-added pip ids must be distinct, got {ids:?}"
        );
    }

    fn zero_zero_creature() -> TokenCharacteristics {
        TokenCharacteristics {
            display_name: "Test Token".to_string(),
            power: Some(0),
            toughness: Some(0),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
            supertypes: Vec::new(),
            colors: vec![ManaColor::Green],
            keywords: Vec::<Keyword>::new(),
        }
    }

    fn prepare_back_face() -> BackFaceData {
        let mut card_types = crate::types::card_type::CardType::default();
        card_types.core_types.push(CoreType::Sorcery);
        BackFaceData {
            name: "Test Prepare Face".to_string(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            card_types,
            mana_cost: ManaCost::default(),
            keywords: Vec::new(),
            abilities: vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )],
            trigger_definitions: Definitions::default(),
            replacement_definitions: Definitions::default(),
            static_definitions: Definitions::default(),
            color: Vec::new(),
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: Vec::new(),
            casting_options: Vec::new(),
            layout_kind: Some(LayoutKind::Prepare),
        }
    }

    /// CR 122.6a + CR 614.1: A debug-created 0/0 creature token with
    /// `+1/+1` counters in `enter_with_counters` enters as a 2/2 because
    /// the counters apply during the same ETB replacement window that
    /// engine-driven token creation uses. CR 704.5f does not kill it.
    /// CR 111.4 + CR 603.6a: Debug preset spawns must install catalog
    /// `rules_text` triggers and register them in the trigger index — same as
    /// engine-driven token creation (issue #853).
    #[test]
    fn debug_create_preset_token_installs_catalog_triggers() {
        let mut state = sandbox_state();
        let sos_pest_preset_id = "00a0801d-0212-5890-8957-3cde30f382f9";
        let action = GameAction::Debug(DebugAction::CreateToken {
            request: DebugTokenRequest::Preset {
                preset_id: sos_pest_preset_id.to_string(),
                owner: PlayerId(0),
                power_override: None,
                toughness_override: None,
                enter_with_counters: Vec::new(),
            },
            run_etb: true,
        });
        let result = crate::game::engine::apply(&mut state, PlayerId(0), action)
            .expect("debug CreateToken preset should succeed");

        let token_id = result
            .events
            .iter()
            .find_map(|event| match event {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .expect("TokenCreated event should fire");

        let obj = state
            .objects
            .get(&token_id)
            .expect("pest token should exist on battlefield");
        assert_eq!(
            obj.trigger_definitions.len(),
            1,
            "SOS Pest preset must install its attack-life trigger"
        );
        assert_eq!(
            obj.trigger_definitions[0].definition.mode,
            crate::types::triggers::TriggerMode::Attacks
        );
        assert!(
            state
                .trigger_index
                .by_key
                .values()
                .any(|bucket| bucket.contains(&token_id)),
            "catalog trigger must be registered in the trigger index"
        );
    }

    #[test]
    fn debug_create_source_defined_preset_requires_both_pt_overrides() {
        let mut state = sandbox_state();
        let action = GameAction::Debug(DebugAction::CreateToken {
            request: DebugTokenRequest::Preset {
                preset_id: "1545ee29-d9c1-57ff-acae-431cfd6d60cf".to_string(),
                owner: PlayerId(0),
                power_override: Some(4),
                toughness_override: None,
                enter_with_counters: Vec::new(),
            },
            run_etb: true,
        });

        let err = crate::game::engine::apply(&mut state, PlayerId(0), action)
            .expect_err("source-defined preset must reject incomplete P/T overrides");

        assert!(format!("{err:?}").contains("requires both power_override and toughness_override"));
    }

    #[test]
    fn debug_create_source_defined_preset_accepts_pt_overrides() {
        let mut state = sandbox_state();
        let action = GameAction::Debug(DebugAction::CreateToken {
            request: DebugTokenRequest::Preset {
                preset_id: "1545ee29-d9c1-57ff-acae-431cfd6d60cf".to_string(),
                owner: PlayerId(0),
                power_override: Some(4),
                toughness_override: Some(5),
                enter_with_counters: Vec::new(),
            },
            run_etb: true,
        });
        let result = crate::game::engine::apply(&mut state, PlayerId(0), action)
            .expect("complete source-defined P/T overrides should create token");

        let token_id = result
            .events
            .iter()
            .find_map(|event| match event {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .expect("TokenCreated event should fire");
        let token = state.objects.get(&token_id).expect("token remains live");

        assert_eq!(token.power, Some(4));
        assert_eq!(token.toughness, Some(5));
    }

    #[test]
    fn debug_create_fixed_preset_rejects_pt_overrides() {
        let mut state = sandbox_state();
        let action = GameAction::Debug(DebugAction::CreateToken {
            request: DebugTokenRequest::Preset {
                preset_id: "25b62fd5-b036-5c64-88fd-8f50d0675e4d".to_string(),
                owner: PlayerId(0),
                power_override: Some(4),
                toughness_override: Some(5),
                enter_with_counters: Vec::new(),
            },
            run_etb: true,
        });

        let err = crate::game::engine::apply(&mut state, PlayerId(0), action)
            .expect_err("fixed preset must reject P/T overrides");

        assert!(format!("{err:?}").contains("does not accept overrides"));
    }

    #[test]
    fn debug_create_token_enters_with_counters_survives_sba() {
        let mut state = sandbox_state();
        let action = GameAction::Debug(DebugAction::CreateToken {
            request: DebugTokenRequest::Custom {
                owner: PlayerId(0),
                characteristics: zero_zero_creature(),
                enter_with_counters: vec![(CounterType::Plus1Plus1, 2)],
            },
            run_etb: true,
        });
        let result = crate::game::engine::apply(&mut state, PlayerId(0), action)
            .expect("debug CreateToken should succeed");

        let token_id = result
            .events
            .iter()
            .find_map(|e| match e {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .expect("TokenCreated event should fire");

        let obj = state
            .objects
            .get(&token_id)
            .expect("token should still exist on battlefield after SBA");
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(
            obj.counters.get(&CounterType::Plus1Plus1).copied(),
            Some(2),
            "token should carry the 2 +1/+1 counters supplied at create-time",
        );
    }

    #[test]
    fn debug_proliferate_starts_real_choice() {
        let mut state = sandbox_state();
        let object_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Bearer".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&object_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::Proliferate {
                player_id: PlayerId(0),
            }),
        )
        .expect("debug Proliferate should succeed");

        assert!(matches!(
            result.waiting_for,
            WaitingFor::ProliferateChoice {
                player: PlayerId(0),
                ..
            }
        ));
        if let WaitingFor::ProliferateChoice { eligible, .. } = result.waiting_for {
            assert!(eligible.contains(&TargetRef::Object(object_id)));
        }
    }

    #[test]
    fn debug_create_token_copy_uses_copy_resolver() {
        let mut state = sandbox_state();
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Copy Source".to_string(),
            Zone::Battlefield,
        );
        let source = state.objects.get_mut(&source_id).unwrap();
        source.base_card_types.core_types.push(CoreType::Creature);
        source.card_types.core_types.push(CoreType::Creature);
        source.base_power = Some(2);
        source.power = Some(2);
        source.base_toughness = Some(3);
        source.toughness = Some(3);

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::CreateTokenCopy {
                source_id,
                owner: PlayerId(1),
            }),
        )
        .expect("debug CreateTokenCopy should succeed");

        let token_id = result
            .events
            .iter()
            .find_map(|event| match event {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .expect("TokenCreated event should fire");
        let token = state
            .objects
            .get(&token_id)
            .expect("copy token should exist");

        assert!(token.is_token);
        assert_eq!(token.controller, PlayerId(1));
        assert_eq!(token.name, "Copy Source");
        assert_eq!(token.power, Some(2));
        assert_eq!(token.toughness, Some(3));
    }

    #[test]
    fn debug_set_prepared_routes_through_prepare_gate() {
        let mut state = sandbox_state();
        let object_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Permanent".to_string(),
            Zone::Battlefield,
        );

        let no_face_result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::SetPrepared {
                object_id,
                prepared: true,
            }),
        )
        .expect("debug SetPrepared should be accepted");
        assert!(state.objects[&object_id].prepared.is_none());
        assert!(!no_face_result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::BecamePrepared { .. })));

        state.objects.get_mut(&object_id).unwrap().back_face = Some(prepare_back_face());

        let prepared_result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::SetPrepared {
                object_id,
                prepared: true,
            }),
        )
        .expect("debug SetPrepared should prepare eligible object");
        assert!(state.objects[&object_id].prepared.is_some());
        assert!(prepared_result.events.iter().any(
            |event| matches!(event, GameEvent::BecamePrepared { object_id: id } if *id == object_id)
        ));

        let unprepared_result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::SetPrepared {
                object_id,
                prepared: false,
            }),
        )
        .expect("debug SetPrepared should unprepare object");
        assert!(state.objects[&object_id].prepared.is_none());
        assert!(unprepared_result.events.iter().any(
            |event| matches!(event, GameEvent::BecameUnprepared { object_id: id } if *id == object_id)
        ));
    }

    /// Issue #464 — CR 110.2 + CR 613.1b: `DebugAction::SetController` must
    /// change a permanent's effective controller AND survive re-evaluation of
    /// the layer system. Controller is a Layer-2 derived property:
    /// `evaluate_layers` resets `obj.controller` to `base_controller` on every
    /// pass. Pre-fix the handler wrote only the derived field, so the next
    /// layer pass reverted control to the owner. The discriminating assertion
    /// is step (b): control must PERSIST across a second `evaluate_layers`.
    #[test]
    fn debug_set_controller_survives_layer_reevaluation() {
        use crate::game::layers::evaluate_layers;
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;

        let mut state = sandbox_state();
        let object_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Permanent".to_string(),
            Zone::Battlefield,
        );
        assert_eq!(state.objects[&object_id].controller, PlayerId(0));

        // A→B: PlayerId(0) → PlayerId(1).
        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::SetController {
                object_id,
                controller: PlayerId(1),
            }),
        )
        .expect("debug SetController should succeed");
        assert_eq!(
            state.objects[&object_id].controller,
            PlayerId(1),
            "effective controller should be the new player immediately",
        );

        // Discriminating assertion: a second layer pass must NOT revert it.
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&object_id].controller,
            PlayerId(1),
            "control must persist across layer re-evaluation (issue #464)",
        );
        assert_eq!(
            state.objects[&object_id].base_controller,
            Some(PlayerId(1)),
            "base_controller is the Layer-2 input that makes the change durable",
        );

        // B→C: transfer control back off the opponent — PlayerId(1) → PlayerId(0).
        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::SetController {
                object_id,
                controller: PlayerId(0),
            }),
        )
        .expect("second debug SetController should succeed");
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&object_id].controller,
            PlayerId(0),
            "control must transfer back and persist across re-evaluation",
        );
    }

    /// CR 613.1 + CR 613.1f: `DebugAction::GrantKeyword`/`RemoveKeyword` must
    /// change a permanent's effective keywords AND survive re-evaluation of the
    /// layer system. Keywords are a Layer-6 derived property: `evaluate_layers`
    /// resets `obj.keywords` to `base_keywords` on every pass. Pre-fix the
    /// handler wrote only the derived field, so the next layer pass dropped the
    /// grant. The discriminating assertion is that the keyword PERSISTS across a
    /// second `evaluate_layers`.
    #[test]
    fn debug_grant_keyword_survives_layer_reevaluation() {
        use crate::game::layers::evaluate_layers;

        let mut state = sandbox_state();
        let object_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Permanent".to_string(),
            Zone::Battlefield,
        );
        assert!(!state.objects[&object_id]
            .keywords
            .contains(&Keyword::Flying));

        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::GrantKeyword {
                object_id,
                keyword: Keyword::Flying,
            }),
        )
        .expect("debug GrantKeyword should succeed");
        assert!(
            state.objects[&object_id]
                .keywords
                .contains(&Keyword::Flying),
            "keyword should be granted immediately",
        );

        // Discriminating assertion: a second layer pass must NOT drop it.
        evaluate_layers(&mut state);
        assert!(
            state.objects[&object_id]
                .keywords
                .contains(&Keyword::Flying),
            "granted keyword must persist across layer re-evaluation",
        );
        assert!(
            state.objects[&object_id]
                .base_keywords
                .contains(&Keyword::Flying),
            "base_keywords is the Layer-6 input that makes the grant durable",
        );

        // Removal must likewise persist across re-evaluation.
        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::RemoveKeyword {
                object_id,
                keyword: Keyword::Flying,
            }),
        )
        .expect("debug RemoveKeyword should succeed");
        evaluate_layers(&mut state);
        assert!(
            !state.objects[&object_id]
                .keywords
                .contains(&Keyword::Flying),
            "removed keyword must stay removed across layer re-evaluation",
        );
    }

    #[test]
    fn debug_move_to_library_honors_position() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;

        let mut state = sandbox_state();
        let existing_top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Existing Top".to_string(),
            Zone::Library,
        );
        let to_top = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Move Top".to_string(),
            Zone::Hand,
        );
        let to_bottom = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Move Bottom".to_string(),
            Zone::Hand,
        );

        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::MoveToZone {
                object_id: to_top,
                to_zone: Zone::Library,
                library_position: Some(LibraryPosition::Top),
                simulate: false,
            }),
        )
        .expect("debug MoveToZone top should succeed");

        assert_eq!(state.players[0].library.front(), Some(&to_top));
        assert_eq!(state.players[0].library.get(1), Some(&existing_top));

        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::MoveToZone {
                object_id: to_bottom,
                to_zone: Zone::Library,
                library_position: Some(LibraryPosition::Bottom),
                simulate: false,
            }),
        )
        .expect("debug MoveToZone bottom should succeed");

        assert_eq!(state.players[0].library.back(), Some(&to_bottom));
    }

    /// Phase D review fix: a `DebugCommand` zone change is FULLY inert — it
    /// skips the delivery tail, not just the replacement consult. Pending ETB
    /// counters from delayed triggers ("that creature enters with an
    /// additional +1/+1 counter") must NOT be applied to or consumed by a
    /// debug-staged battlefield entry. Pre-fix, the exempt path delivered
    /// through the full tail: the staged object entered with the pending
    /// counters and the `pending_etb_counters` entry was consumed (the same
    /// tail arm would also mint Kalain-class `EntersWithAdditionalCounters`
    /// statics onto staged creatures).
    #[test]
    fn debug_move_to_battlefield_skips_delivery_tail_counters() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;

        let mut state = sandbox_state();
        let staged = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Staged Creature".to_string(),
            Zone::Hand,
        );
        state
            .pending_etb_counters
            .push((staged, CounterType::Plus1Plus1, 2));

        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::MoveToZone {
                object_id: staged,
                to_zone: Zone::Battlefield,
                library_position: None,
                simulate: false,
            }),
        )
        .expect("debug MoveToZone battlefield should succeed");

        let obj = &state.objects[&staged];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(
            obj.counters.is_empty(),
            "a debug-staged entry must not receive delivery-tail counters"
        );
        assert_eq!(
            state.pending_etb_counters.len(),
            1,
            "a debug-staged entry must not consume pending ETB counters"
        );
    }

    #[test]
    fn debug_modify_player_counters_routes_poison_to_dedicated_field() {
        let mut state = sandbox_state();

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::ModifyPlayerCounters {
                player_id: PlayerId(1),
                counter_kind: PlayerCounterKind::Poison,
                delta: 3,
            }),
        )
        .expect("debug ModifyPlayerCounters should succeed");

        assert_eq!(state.players[1].poison_counters, 3);
        assert_eq!(
            state.players[1]
                .player_counters
                .get(&PlayerCounterKind::Poison),
            None
        );
        assert!(result.events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerCounterChanged {
                player: PlayerId(1),
                counter_kind: PlayerCounterKind::Poison,
                delta: 3,
            }
        )));
    }

    #[test]
    fn debug_modify_player_counters_routes_generic_kinds_to_map() {
        let mut state = sandbox_state();

        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::ModifyPlayerCounters {
                player_id: PlayerId(0),
                counter_kind: PlayerCounterKind::Experience,
                delta: 2,
            }),
        )
        .expect("debug ModifyPlayerCounters should succeed");

        assert_eq!(
            state.players[0].player_counter(&PlayerCounterKind::Experience),
            2
        );
    }

    #[test]
    fn debug_modify_player_counters_removal_reports_actual_delta() {
        let mut state = sandbox_state();
        state.players[0].add_player_counters(&PlayerCounterKind::Rad, 2);

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::ModifyPlayerCounters {
                player_id: PlayerId(0),
                counter_kind: PlayerCounterKind::Rad,
                delta: -5,
            }),
        )
        .expect("debug ModifyPlayerCounters should succeed");

        assert_eq!(state.players[0].player_counter(&PlayerCounterKind::Rad), 0);
        assert!(result.events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerCounterChanged {
                player: PlayerId(0),
                counter_kind: PlayerCounterKind::Rad,
                delta: -2,
            }
        )));
    }

    #[test]
    fn debug_modify_absent_player_counter_emits_no_event() {
        let mut state = sandbox_state();

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::ModifyPlayerCounters {
                player_id: PlayerId(0),
                counter_kind: PlayerCounterKind::Ticket,
                delta: -1,
            }),
        )
        .expect("debug ModifyPlayerCounters should succeed");

        assert!(!result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::PlayerCounterChanged { .. })));
    }

    #[test]
    fn debug_modify_energy_reports_actual_delta() {
        let mut state = sandbox_state();
        state.players[0].energy = 2;

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::ModifyEnergy {
                player_id: PlayerId(0),
                delta: -5,
            }),
        )
        .expect("debug ModifyEnergy should succeed");

        assert_eq!(state.players[0].energy, 0);
        assert!(result.events.iter().any(|event| matches!(
            event,
            GameEvent::EnergyChanged {
                player: PlayerId(0),
                delta: -2,
            }
        )));
    }

    #[test]
    fn debug_modify_absent_energy_emits_no_event() {
        let mut state = sandbox_state();

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::ModifyEnergy {
                player_id: PlayerId(0),
                delta: -1,
            }),
        )
        .expect("debug ModifyEnergy should succeed");

        assert!(!result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::EnergyChanged { .. })));
    }

    /// CR 704.5f negative control: a debug-created 0/0 creature token
    /// with no counters dies to state-based actions on the same `apply`,
    /// proving the survival in the positive test is due to the counters
    /// and not some unrelated default. Locks in current SBA semantics so
    /// an accidental auto-bump elsewhere can't silently change behavior.
    #[test]
    fn debug_create_token_zero_zero_no_counters_dies_to_sba() {
        let mut state = sandbox_state();
        let action = GameAction::Debug(DebugAction::CreateToken {
            request: DebugTokenRequest::Custom {
                owner: PlayerId(0),
                characteristics: zero_zero_creature(),
                enter_with_counters: Vec::new(),
            },
            run_etb: true,
        });
        let result = crate::game::engine::apply(&mut state, PlayerId(0), action)
            .expect("debug CreateToken should succeed");

        let token_id = result
            .events
            .iter()
            .find_map(|e| match e {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .expect("TokenCreated event should fire");

        // CR 704.5d: Tokens that leave the battlefield cease to exist, so
        // the object should not be present in `state.objects` after SBA.
        assert!(
            !state.objects.contains_key(&token_id),
            "0/0 token with no counters should be removed by SBA + CR 704.5d",
        );
    }

    /// CR 603.2 + CR 121.1: Debug draw must scan CardDrawn events for triggers,
    /// matching the post-priority pipeline that natural draw-step draws use.
    #[test]
    fn debug_draw_cards_processes_draw_triggers() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::types::phase::Phase;
        use crate::types::triggers::TriggerMode;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Lib A", "Lib B"]);
        scenario.add_creature_from_oracle(
            P0,
            "Watcher",
            2,
            2,
            "Whenever you draw a card, you gain 2 life.",
        );
        let mut runner = scenario.build();
        runner.state_mut().debug_mode = true;

        let life_before = runner.state().players[0].life;
        runner
            .act(GameAction::Debug(DebugAction::DrawCards {
                player_id: P0,
                count: 1,
            }))
            .expect("debug draw");
        runner.advance_until_stack_empty();

        assert_eq!(
            runner.state().players[0].life,
            life_before + 2,
            "draw trigger must fire after DebugAction::DrawCards"
        );
        let watcher = runner
            .state()
            .battlefield
            .iter()
            .find_map(|id| runner.state().objects.get(id))
            .expect("watcher on battlefield");
        assert!(
            watcher
                .trigger_definitions
                .iter_all()
                .any(|t| t.definition.mode == TriggerMode::Drawn),
            "sanity: watcher carries a Drawn trigger"
        );
    }
}
