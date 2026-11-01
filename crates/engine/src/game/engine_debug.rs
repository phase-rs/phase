use std::collections::HashSet;

use crate::types::ability::{
    ContinuousModification, Effect, EntryAttackDestination, LibraryPosition, QuantityExpr,
    ResolvedAbility, TargetFilter, TargetRef, TokenAttackDestination,
};
use crate::types::action_rejection::ActionRejection;
use crate::types::actions::{DebugAction, DebugCardCreationKind, DebugTokenRequest, GameAction};
use crate::types::card::CardFace;
use crate::types::card_type::Supertype;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActionResult, DebugCardEntrySource, GameState, LiminalEntry, PendingDebugCardEntries,
    PendingLiminalEntryResume, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::{PlayerCounterKind, PlayerId};
use crate::types::proposed_event::{CopyTokenSpec, ProposedEvent, TokenAttackRequest};
use crate::types::resolved_commands::ResolvedPlayerEdit;
use crate::types::zones::Zone;

use super::effects::attach::{attach_to as attach_object_to, attach_to_player};
use super::effects::change_zone::shuffle_library;
use super::effects::energy::try_add_energy_with_replacement;
use super::effects::life::{
    try_apply_life_gain, try_apply_life_loss, LifeChangeError, ReplacementDeferred,
};
use super::effects::player_counter::{
    try_add_player_counter_with_replacement, PlayerCounterAdditionOutcome,
};
use super::engine::{
    action_rejection_for_engine_error, explicit_debug_permission_rejection, preflight_debug_action,
    EngineError,
};
use super::game_object::AttachTarget;
use super::replacement::{self, ReplacementResult};
use super::visibility::filter_action_rejection_for_viewer;
use super::zones;
use crate::database::CardDatabase;
use crate::game::token_presets::TokenPtProvenance;

pub fn apply_debug_action(
    state: &mut GameState,
    actor: PlayerId,
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
            if simulate && to_zone == Zone::Battlefield {
                // "Run ETB effects": a battlefield entry goes through the real
                // entry pipeline so enters-with/as-enters replacements apply
                // and ETB triggers fire, exactly as a debug-created card does.
                enter_battlefield_with_etb(state, object_id, None, events);
            } else {
                // Debug forces a zone change — route through the zone pipeline under
                // the `DebugCommand` exempt cause, which skips
                // both the replacement consult and the delivery tail (no
                // enters-with-counter statics, no pending-ETB-counter consumption,
                // no devour snapshot); the CR 306.5b / CR 310.4b characteristic
                // counters are installed by the DebugCommand arm, and the
                // unconditional primitive guards still run. DebugCommand is
                // non-pausing by construction (always `Done`), so the result is
                // safely discarded. The library-position
                // arm folds the raw `move_to_library_position` / `_at_index`
                // siblings in via the placement request.
                let mut req =
                    crate::game::zone_pipeline::ZoneMoveRequest::debug(object_id, to_zone);
                if to_zone == Zone::Library {
                    req = req
                        .at_library_position(library_position.unwrap_or(LibraryPosition::Bottom));
                }
                crate::game::zone_pipeline::move_object(state, req, events);
                if simulate {
                    super::sba::check_state_based_actions(state, events);
                    super::triggers::process_triggers(state, events);
                }
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
            let top_ids: Vec<ObjectId> = state
                .library_of(player_id)
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
                let remove_counter = if let Some(entry) = obj.counters.get_mut(&counter_type) {
                    *entry = entry.saturating_sub(delta.unsigned_abs());
                    *entry == 0
                } else {
                    false
                };
                if remove_counter {
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
            // CR 701.26a-b: Debug actions use the same checked status authority.
            crate::game::object_state::resolve_and_apply_object_edit(
                state,
                object_id,
                crate::types::resolved_commands::ResolvedObjectStatus::Tapped,
                tapped,
            )
            .map_err(|err| EngineError::InvalidAction(format!("{err:?}")))?;
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
                let (zone, was_face_down, has_stored_face, controller) = {
                    let obj = state.objects.get(&object_id).unwrap();
                    (
                        obj.zone,
                        obj.face_down,
                        obj.back_face.is_some(),
                        obj.controller,
                    )
                };
                // CR 702.37e + CR 708.2a: turning a permanent face up must
                // RESTORE the stored face, not just clear the flag — the same
                // class as the `transformed` arm below, and for the same reason.
                // A flag-only write leaves the CR 708.2a vanilla 2/2 installed
                // (no name, no abilities, no printed P/T), so the tool appears to
                // do nothing, no CR 613.7f timestamp is drawn, the
                // "as ~ is turned face up" replacement never applies, and no
                // `TurnedFaceUp` event reaches the triggers (#7539).
                //
                // `morph::turn_face_up` is that single authority, shared with the
                // paid `GameAction::TurnFaceUp` special action and the free
                // effect callers, so the tool cannot drift from either. It also
                // owns the CR 701.40b legality question (a manifested card is
                // turned up only if it is a creature card with a mana cost), and
                // reports it as an error rather than silently doing nothing.
                let on_battlefield = zone == Zone::Battlefield;
                match (fd, was_face_down) {
                    // Turn face up: restore the stored face.
                    (false, true) if on_battlefield && has_stored_face => {
                        crate::game::morph::turn_face_up(state, controller, object_id, events)?;
                    }
                    // CR 708.2a: turning a permanent face down must SNAPSHOT
                    // the real face and install the 2/2 in its place. The flag
                    // alone leaves the permanent with its name, printed P/T and
                    // abilities while claiming to be face down — and `back_face`
                    // stays empty, so the arm above can never bring it back
                    // (#7541).
                    //
                    // `effects::turn_face_down::turn_permanent_face_down` is
                    // the direct-turn authority (shared with the Ixidron /
                    // Cyber Conversion resolver), NOT the battlefield-entry
                    // profile: a permanent already on the battlefield needs the
                    // BASE-face snapshot (a live snapshot bakes active
                    // continuous modifications into the restored card), keeps a
                    // flipped permanent's stashed normal half, refuses
                    // double-faced and melded permanents (CR 712.16 /
                    // CR 730.2j), and emits the `TurnedFaceDown` event the
                    // triggers observe.
                    //
                    // CR 708.2b — "A face-down permanent can't be turned face
                    // down. If a spell or ability attempts to turn a face-down
                    // permanent face down, nothing happens" — falls out of the
                    // `was_face_down` guard rather than being re-asserted.
                    (true, false) if on_battlefield => {
                        // The guard already excludes the face-down case, so a
                        // refusal here is the CR 712.16 / CR 730.2j class.
                        // Report it, mirroring the face-up arm's error stance,
                        // rather than silently doing nothing.
                        if !crate::game::effects::turn_face_down::turn_permanent_face_down(
                            state,
                            object_id,
                            &crate::types::ability::FaceDownProfile::vanilla_2_2()
                                .caused_by(crate::types::ability::FaceDownCause::TurnedFaceDown),
                            events,
                        ) {
                            return Err(EngineError::InvalidAction(
                                "Debug: a double-faced or melded permanent can't be turned \
                                 face down (CR 712.16 / CR 730.2j)"
                                    .to_string(),
                            ));
                        }
                    }
                    // Everything else is a flag write with nothing to move: the
                    // object is not on the battlefield (no permanent exists to
                    // turn), it is already in the requested state, or it is face
                    // down with no stored face for `turn_face_up` to restore.
                    _ => {
                        validate_object_mut(state, object_id)?.face_down = fd;
                    }
                }
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
                    attach_object_to(state, object_id, target_id);
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
            obj.restore_token_art_baseline();
            crate::game::layers::mark_layers_full(state);
        }

        DebugAction::RemoveKeyword { object_id, keyword } => {
            let obj = validate_object_mut(state, object_id)?;
            // CR 613.1 + CR 613.1f: write the base keyword set (the Layer-6 input)
            // so the removal survives the layer recompute; see GrantKeyword above.
            obj.base_keywords.retain(|k| k != &keyword);
            obj.restore_token_art_baseline();
            crate::game::layers::mark_layers_full(state);
        }

        DebugAction::SetLife { player_id, life } => {
            // CR 119.5: Setting life gains or loses the required semantic delta.
            validate_player(state, player_id)?;
            let current_life = state
                .players
                .iter()
                .find(|player| player.id == player_id)
                .expect("the validated debug player must remain present")
                .life;
            if current_life != life {
                let delta = life.checked_sub(current_life).ok_or_else(|| {
                    EngineError::InvalidAction("Debug: life delta is not representable".to_string())
                })?;
                state
                    .resolve_and_apply_player_edit(player_id, ResolvedPlayerEdit::Life { delta })
                    .map_err(|err| EngineError::InvalidAction(format!("{err:?}")))?;
            }
        }

        DebugAction::GainLife { player_id, amount } => {
            apply_debug_life_change(state, player_id, amount, DebugLifeChange::Gain, events)?;
        }

        DebugAction::LoseLife { player_id, amount } => {
            apply_debug_life_change(state, player_id, amount, DebugLifeChange::Loss, events)?;
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

        DebugAction::GivePlayerCounters {
            player_id,
            counter_kind,
            count,
        } => {
            apply_debug_counter_gain(
                state,
                actor,
                player_id,
                DebugCounterGain::Counter(counter_kind),
                count,
                events,
            )?;
        }

        DebugAction::GiveEnergy { player_id, count } => {
            apply_debug_counter_gain(
                state,
                actor,
                player_id,
                DebugCounterGain::Energy,
                count,
                events,
            )?;
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
                state.mark_unbounded_loop(
                    player_id,
                    &super::mana_payment::INFINITE_MANA_AXES,
                    &super::mana_payment::INFINITE_MANA_AXES,
                );
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
            // CR 701.20a: every reveal lease is bound to a stack entry that
            // this jump just removed.
            state.release_all_stack_bound_reveals();
            state.waiting_for = WaitingFor::Priority {
                player: active_player,
            };
        }

        DebugAction::RunStateBasedActions => {
            super::sba::check_state_based_actions(state, events);
            super::triggers::process_triggers(state, events);
        }

        DebugAction::CreateToken {
            request,
            count,
            run_etb,
        } => {
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
                attack_request: TokenAttackRequest::Unspecified,
                sacrifice_at: None,
                source_id: ObjectId(0),
                controller: owner,
                attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
            };
            let proposed = ProposedEvent::CreateToken {
                owner,
                spec: Box::new(spec),
                copy: None,
                enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
                count,
                appended: Vec::new(),
                applied: HashSet::new(),
            };
            match super::replacement::replace_event(state, proposed, events) {
                super::replacement::ReplacementResult::Execute(event) => {
                    let delivery = super::effects::token::deliver_create_token_event(
                        state,
                        event,
                        Vec::new(),
                        crate::types::game_state::PendingEffectResolutionEvent::Emit,
                        events,
                    );
                    // CR 111.4 + CR 707.2a: Preset spawns must install catalog
                    // `rules_text` abilities (SOS Pest attack-life trigger, etc.)
                    // after linking the preset image ref. The apply path runs
                    // `inject_catalog_token_abilities` during creation when
                    // `token_image_ref` is already set; debug preset creation
                    // deferred the ref until here, so inject + reindex now.
                    //
                    // CR 111.4 + CR 614.1a: the preset describes the requested
                    // token only. A replacement-appended batch (Chatterfang's
                    // Squirrels) is a different token and keeps its own identity —
                    // the Squirrels "won't have any abilities the other tokens
                    // were created with" — so only the event's own batch is
                    // stamped.
                    if let Some(image_ref) = preset_image_ref {
                        let created_ids = delivery.primary_ids().to_vec();
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

        DebugAction::CreateTokenCopy {
            source_id,
            owner,
            count,
            nonlegendary,
        } => {
            validate_object(state, source_id)?;
            validate_player(state, owner)?;
            let ability = ResolvedAbility::new(
                Effect::CopyTokenOf {
                    target: TargetFilter::Any,
                    owner: TargetFilter::Controller,
                    source_filter: None,
                    enters_attacking: false,
                    attack_destination: TokenAttackDestination::ControllerChooses,
                    tapped: false,
                    count: QuantityExpr::Fixed {
                        value: i32::try_from(count)
                            .expect("debug create count is bounded below i32::MAX"),
                    },
                    extra_keywords: vec![],
                    additional_modifications: nonlegendary
                        .then_some(ContinuousModification::RemoveSupertype {
                            supertype: Supertype::Legendary,
                        })
                        .into_iter()
                        .collect(),
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

    Ok(ActionResult::applied(
        std::mem::take(events),
        state.waiting_for.clone(),
    ))
}

/// CR 119.3: Direction of a sandbox life change routed through the real life-change event.
#[derive(Debug, Clone, Copy)]
enum DebugLifeChange {
    Gain,
    Loss,
}

/// CR 119.3: Apply a sandbox life gain/loss through `try_apply_life_gain` /
/// `try_apply_life_loss`, so replacement (CR 614.1a, CR 616.1) and can't (CR 101.2)
/// effects apply, then settle the action the way the reducer settles any action.
fn apply_debug_life_change(
    state: &mut GameState,
    player_id: PlayerId,
    amount: u32,
    change: DebugLifeChange,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    validate_player(state, player_id)?;
    if !super::players::is_alive(state, player_id) {
        return Err(EngineError::InvalidAction(format!(
            "Debug: player {} is no longer in the game",
            player_id.0
        )));
    }
    let event_start = events.len();
    // The final edit is the single representability authority: an unrepresentable
    // result (after any replacement) rejects the action and the action boundary
    // restores its snapshot — no wrap, no clamp, nothing settles.
    let outcome = match change {
        DebugLifeChange::Gain => try_apply_life_gain(state, player_id, amount, events),
        DebugLifeChange::Loss => try_apply_life_loss(state, player_id, amount, events),
    };
    match outcome {
        Ok(_) => {
            // CR 603.2 + CR 603.3 + CR 704.3: scan this change's events for
            // triggers, run SBAs, and scan the SBA-generated events (dies triggers,
            // target prompts) through the reducer's single settlement authority.
            // The Debug branch returns before `run_post_action_pipeline`, so the
            // arm converges here over exactly its own slice.
            let default_wf = state.waiting_for.clone();
            let waiting_for = super::engine_priority::run_post_action_pipeline_from(
                state,
                events,
                event_start,
                &default_wf,
                false,
                false,
            )?;
            state.waiting_for = waiting_for;
        }
        // CR 616.1: the authority installed the replacement-order prompt; its
        // resume path owns the event's triggers and SBAs.
        Err(LifeChangeError::Deferred(ReplacementDeferred::ReplacementChoice)) => {}
        // CR 614.6 + CR 603.7: the substitute is paused on its own prompt. Queue
        // (never dispatch) ordinary and delayed triggers for the resume's drain;
        // no SBAs mid-resolution.
        Err(LifeChangeError::Deferred(ReplacementDeferred::SubstitutionContinuation {
            ..
        })) => {
            super::triggers::collect_triggers_into_deferred(state, &events[event_start..]);
            super::triggers::collect_delayed_trigger_contexts_into_deferred(
                state,
                &events[event_start..],
            );
        }
        Err(LifeChangeError::Unrepresentable(unrepresentable)) => {
            return Err(unrepresentable.into());
        }
    }
    Ok(())
}

/// CR 122.1: Which player resource a sandbox give places.
#[derive(Debug, Clone, Copy)]
enum DebugCounterGain {
    Counter(PlayerCounterKind),
    /// CR 107.14: energy counters, stored apart from `PlayerCounterKind`.
    Energy,
}

/// CR 122.1: Give a player counters or energy through the action-ingress
/// counter-placement authority, so can't effects (CR 614.17), count
/// replacements (CR 614.1) and CR 616.1 ordering apply, then settle the
/// action the way the reducer settles any action. The submitting seat
/// (`actor`) is the player who puts the counters; `player_id` receives them.
fn apply_debug_counter_gain(
    state: &mut GameState,
    actor: PlayerId,
    player_id: PlayerId,
    gain: DebugCounterGain,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    validate_player(state, player_id)?;
    if !super::players::is_alive(state, player_id) {
        return Err(EngineError::InvalidAction(format!(
            "Debug: player {} is no longer in the game",
            player_id.0
        )));
    }
    let event_start = events.len();
    // The settle step is the single representability authority: an
    // unrepresentable result (after any replacement) rejects the action and
    // the action boundary restores its snapshot. No wrap, no clamp.
    let parked = match gain {
        DebugCounterGain::Counter(kind) => {
            match try_add_player_counter_with_replacement(
                state, actor, player_id, kind, count, events,
            )? {
                PlayerCounterAdditionOutcome::Applied | PlayerCounterAdditionOutcome::Prevented => {
                    false
                }
                PlayerCounterAdditionOutcome::NeedsChoice => true,
            }
        }
        // `true` = settled (applied or prevented), `false` = parked.
        DebugCounterGain::Energy => {
            !try_add_energy_with_replacement(state, actor, player_id, count, events)?
        }
    };
    // CR 616.1: a parked ordering choice owns the rest of the event; its
    // resume runs the event's triggers and state-based actions.
    if parked {
        return Ok(());
    }
    // CR 603.2 + CR 603.3 + CR 704.3: scan this give's events for triggers
    // ("whenever you get {E}"), run SBAs (CR 704.5c: ten or more poison
    // counters), and scan the SBA events, through the reducer's single
    // settlement authority, over exactly this arm's slice.
    let default_wf = state.waiting_for.clone();
    let waiting_for = super::engine_priority::run_post_action_pipeline_from(
        state,
        events,
        event_start,
        &default_wf,
        false,
        false,
    )?;
    state.waiting_for = waiting_for;
    Ok(())
}

/// CR 122.1: Apply a final debug-selected player-counter delta through the
/// same scalar authority as ordinary rules actions.
fn apply_player_counter_delta(
    state: &mut GameState,
    player_id: PlayerId,
    counter_kind: PlayerCounterKind,
    delta: i32,
    events: &mut Vec<GameEvent>,
) {
    let Some(before) = state
        .players
        .iter()
        .find(|player| player.id == player_id)
        .map(|player| player.player_counter(&counter_kind))
    else {
        return;
    };
    let after = if delta.is_positive() {
        before
            .checked_add(delta as u32)
            .expect("debug counter addition must not overflow")
    } else {
        before.saturating_sub(delta.unsigned_abs())
    };
    let actual_delta = i32::try_from(i64::from(after) - i64::from(before))
        .expect("a requested i32 counter delta must remain representable");
    if actual_delta != 0 {
        state
            .resolve_and_apply_player_edit(
                player_id,
                ResolvedPlayerEdit::Counter {
                    kind: counter_kind,
                    delta: actual_delta,
                },
            )
            .expect("the computed debug counter delta must satisfy its resolved precondition");
        events.push(GameEvent::PlayerCounterChanged {
            player: player_id,
            counter_kind,
            delta: actual_delta,
        });
    }
}

/// CR 107.14 + CR 122.1: Apply a final debug-selected energy-counter delta
/// through the same scalar authority as ordinary rules actions.
fn apply_energy_delta(
    state: &mut GameState,
    player_id: PlayerId,
    delta: i32,
    events: &mut Vec<GameEvent>,
) {
    let Some(before) = state
        .players
        .iter()
        .find(|player| player.id == player_id)
        .map(|player| player.energy)
    else {
        return;
    };
    let after = if delta.is_positive() {
        before
            .checked_add(delta as u32)
            .expect("debug energy addition must not overflow")
    } else {
        before.saturating_sub(delta.unsigned_abs())
    };
    let actual_delta = i32::try_from(i64::from(after) - i64::from(before))
        .expect("a requested i32 energy delta must remain representable");
    if actual_delta != 0 {
        state
            .resolve_and_apply_player_edit(
                player_id,
                ResolvedPlayerEdit::Energy {
                    delta: actual_delta,
                },
            )
            .expect("the computed debug energy delta must satisfy its resolved precondition");
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
    attach_to: Option<AttachTarget>,
) -> ActionResult {
    let mut events: Vec<GameEvent> = vec![];

    // "Run ETB effects" unchecked: place the staged object on the battlefield
    // raw — no replacement window, no ETB triggers, no SBA pass. This mirrors
    // `MoveToZone { simulate: false }`, letting a board position be staged
    // without the entering permanent's "when ~ enters" abilities going on the
    // stack.
    if !run_etb {
        // Debug staging — route through the pipeline under `DebugCommand`,
        // which makes no replacement consult AND runs no delivery tail
        // (no replacement-derived or statics-derived enters-with counters, no
        // pending-ETB-counter consumption, no devour snapshot); the CR 306.5b
        // / CR 310.4b characteristic counters are installed by the
        // DebugCommand arm. ETB triggers / SBA are NOT run here;
        // that is `run_etb`'s job below. DebugCommand is non-pausing by
        // construction (always `Done`), so the result is safely discarded.
        let req = crate::game::zone_pipeline::ZoneMoveRequest::debug(object_id, Zone::Battlefield);
        crate::game::zone_pipeline::move_object(state, req, &mut events);
        crate::game::layers::mark_layers_full(state);
        return ActionResult::applied(events, state.waiting_for.clone());
    }

    if state
        .objects
        .get(&object_id)
        .is_some_and(|object| object.is_token)
    {
        return route_debug_token_to_battlefield(state, object_id, attach_to);
    }

    enter_battlefield_with_etb(state, object_id, attach_to, &mut events);
    ActionResult::applied(events, state.waiting_for.clone())
}

/// CR 614.12 + CR 603.6a: Move an existing object onto the battlefield through
/// the real entry pipeline — the replacement consult (enters tapped, enters
/// with counters, including the CR 306.5b / CR 310.4b intrinsic
/// loyalty/defense counters, "as enters" choices), delivery, ETB triggers,
/// then SBAs. A replacement choice parks on `state.waiting_for`. Shared by debug card
/// creation and `MoveToZone { simulate: true }`. A move the zone primitive
/// rejects as a no-op — an object already on the battlefield (CR 400.6 +
/// CR 603.6d), or a token that has left it (CR 111.8) — builds no proposal
/// (nothing is seeded, consulted or delivered) and runs only the trigger / SBA
/// epilogue, through `zone_pipeline::zone_move_is_primitive_noop`, the guard the
/// zone pipeline hoists ahead of its own consult.
fn enter_battlefield_with_etb(
    state: &mut GameState,
    object_id: ObjectId,
    attach_to: Option<AttachTarget>,
    events: &mut Vec<GameEvent>,
) {
    let Some(object) = state.objects.get(&object_id) else {
        return;
    };
    let from = object.zone;
    // CR 400.6 + CR 603.6d + CR 111.8: an object already on the battlefield is
    // not put onto it, and a token that has left the battlefield can't return —
    // the pipeline's own pre-consult guard, so nothing is seeded, consulted or
    // delivered for a move the zone primitive rejects. Like every other simulated
    // move, triggers and state-based actions are still checked, so a stranded
    // token ceases to exist (CR 704.5d).
    if crate::game::zone_pipeline::zone_move_is_primitive_noop(state, object, Zone::Battlefield) {
        super::triggers::process_triggers(state, events); // CR 603: Process triggers
        super::sba::check_state_based_actions(state, events); // CR 704: Check SBAs
        return;
    }

    let mut proposed = ProposedEvent::ZoneChange {
        object_id,
        from,
        to: Zone::Battlefield,
        cause: None,
        attach_to,
        enter_tapped: Default::default(),
        enters_attacking: false,
        enter_with_counters: vec![],
        controller_override: None,
        enter_transformed: false,
        face_down_profile: None,
        face_down_in_exile: crate::types::ability::ExileConcealment::Public,
        chain_referent: crate::types::zones::ChainReferentIntent::Silent,
        enter_as_copy: None,
        enter_with_choices: None,
        discard_frame: None,
        performed_by: None,
        applied: HashSet::new(),
    };
    // CR 306.5b + CR 310.4b + CR 614.1c: seed the intrinsic enters-with
    // counters the effect-driven pipeline seeds, so the replacement consult
    // sees them and the delivery tail applies Doubling Season class
    // replacements (CR 614.1a) to them.
    crate::game::zone_pipeline::seed_intrinsic_entry_counters(state, &mut proposed);

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            // CR 614.12a: a Devour as-enters sacrifice may surface its own
            // `EffectZoneChoice`; park on it so the debug-place flow keeps the
            // pending sacrifice prompt instead of overwriting it.
            match super::effects::change_zone::deliver_replaced_zone_change(
                state,
                event,
                None,
                None,
                None,
                false,
                crate::types::game_state::PostReplacementDrainOwner::DeliveryTail,
                None,
                events,
            ) {
                super::effects::change_zone::ZoneDeliveryResult::Done => {}
                super::effects::change_zone::ZoneDeliveryResult::NeedsChoice(player) => {
                    replacement::park_waiting_for(state, player);
                }
            }
            super::triggers::process_triggers(state, events); // CR 603: Process triggers
            super::sba::check_state_based_actions(state, events); // CR 704: Check SBAs
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
        }
    }
}

/// CR 111.1 + CR 614.12: A token does not move from a staging zone onto the
/// battlefield. Convert the debug-selected printed characteristics into the
/// same liminal token projection used by copy-token effects, then consult the
/// standard token-entry replacement and delivery pipeline.
fn route_debug_token_to_battlefield(
    state: &mut GameState,
    object_id: ObjectId,
    attach_to: Option<AttachTarget>,
) -> ActionResult {
    let mut events = Vec::new();
    let staged = state
        .objects
        .remove(&object_id)
        .expect("debug token must exist before its entry is staged");
    // allow-raw-zone: removes a private debug staging row before its CR 111.1 no-from-zone token entry; no game event may observe the staging zone.
    zones::remove_from_zone(state, object_id, staged.zone, staged.owner);

    let values = super::printed_cards::intrinsic_copiable_values(&staged);
    // CR 306.5b + CR 310.4b + CR 614.1c: the sandbox token's body is the
    // card's copiable printed characteristics; its printed loyalty and the
    // staged card's printed defense (CopiableValues carries no defense) seed
    // the intrinsic loyalty/defense counters onto the TokenEntry proposal,
    // which the liminal commit applies through the replacement pipeline
    // (CR 614.1a). The production copy-token path seeds the same counters
    // itself and uses this liminal seam only when it has none.
    let intrinsic_counters = super::printed_cards::intrinsic_face_entry_counters(
        values.printed_loyalty,
        values.loyalty,
        None,
        staged.base_defense,
    );
    let copy = CopyTokenSpec {
        values: Box::new(values.clone()),
        display: super::game_object::DisplayIdentity {
            token_art: None,
            ..staged.display_identity()
        },
        extra_keywords: Vec::new(),
        additional_modifications: Vec::new(),
        tapped: false,
        enters_attacking: false,
        attack_request: TokenAttackRequest::Unspecified,
        sacrifice_at: None,
        source_id: object_id,
        controller: staged.controller,
        // Debug-token route stages no donor; empty mods are stamp-proof.
        donor: None,
    };
    let mut token = super::game_object::GameObject::new(
        object_id,
        CardId(0),
        staged.owner,
        values.name.clone(),
        Zone::Battlefield,
    );
    let entry_timestamp = state.next_timestamp();
    super::effects::token::materialize_token_copy_body(
        &mut token,
        &copy,
        &crate::types::resolved_commands::ResolvedCopyBodyModifications::NoExceptions,
        state.turn_number,
        entry_timestamp,
        false,
    );
    state.liminal_entries.insert(
        object_id,
        LiminalEntry {
            object: crate::types::game_state::LiminalEntrant::Token(
                crate::types::game_state::TokenProjection::materialize(token),
            ),
            name: values.name,
            source_id: object_id,
            controller: staged.controller,
            enters_attacking: false,
            attack_destination: EntryAttackDestination::AnyDefender,
            attach_to,
            sacrifice_at: None,
            remaining_count: 0,
            created_ids: Vec::new(),
            copy_resume: Some(Box::new(copy)),
            spec_resume: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enter_with_counters: Vec::new(),
            kind: crate::types::game_state::LiminalEntryKind::Token,
            replacement_applied: HashSet::new(),
        },
    );

    let proposed = ProposedEvent::TokenEntry {
        entry_ref: object_id,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enter_with_counters: intrinsic_counters,
        enter_with_choices: None,
        applied: HashSet::new(),
    };
    match replacement::replace_event(state, proposed, &mut events) {
        ReplacementResult::Execute(event) => {
            if state.has_post_replacement_drain() {
                if let Some(waiting_for) =
                    super::engine_replacement::apply_pending_post_replacement_effect(
                        state,
                        Some(object_id),
                        None,
                        Some(crate::types::replacements::ReplacementEvent::Moved),
                        &mut events,
                    )
                {
                    state.pending_liminal_entry_resume = Some(PendingLiminalEntryResume::Token {
                        source_id: object_id,
                        player: waiting_for.acting_player().unwrap_or(staged.controller),
                        event,
                    });
                    state.waiting_for = waiting_for;
                    return ActionResult::applied(events, state.waiting_for.clone());
                }
            }
            if super::effects::token::commit_liminal_token_entry_and_continue_copy_batch(
                state,
                event,
                &mut events,
            ) {
                super::triggers::process_triggers(state, &events);
                super::sba::check_state_based_actions(state, &mut events);
            }
        }
        ReplacementResult::Prevented => {
            state.liminal_entries.remove(&object_id);
        }
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
        }
    }

    ActionResult::applied(events, state.waiting_for.clone())
}

/// Bind a debug card request to its complete printed characteristics before a
/// batch can pause. The source can then survive save/restore without a later
/// lookup through the adapter-owned card database.
pub fn debug_card_entry_source(db: &CardDatabase, face: &CardFace) -> DebugCardEntrySource {
    DebugCardEntrySource {
        face: face.clone(),
        back_face: super::printed_cards::back_face_for_card_face(db, face),
        outside_game_faces: super::printed_cards::outside_game_faces_for(face, db),
    }
}

/// Engine input for one debug Create Card request after the transport has
/// resolved its requested name into a face-complete private source.
#[derive(Debug, Clone)]
pub struct DebugCardCreateRequest {
    pub actor: PlayerId,
    pub source: DebugCardEntrySource,
    pub owner: PlayerId,
    pub zone: Zone,
    pub count: u32,
    pub attach_to: Option<AttachTarget>,
    pub run_etb: bool,
    pub nonlegendary: bool,
    pub creation_kind: DebugCardCreationKind,
}

impl DebugCardCreateRequest {
    pub(crate) fn as_debug_action(&self) -> DebugAction {
        DebugAction::CreateCard {
            card_name: self.source.face.name.clone(),
            owner: self.owner,
            zone: self.zone,
            count: self.count,
            attach_to: self.attach_to,
            run_etb: self.run_etb,
            nonlegendary: self.nonlegendary,
            creation_kind: self.creation_kind,
        }
    }
}

/// Create one or more debug cards from a previously bound source. Non-
/// battlefield creation and explicitly raw battlefield placement complete
/// synchronously. Real battlefield entries drain serially through the private
/// resolution frame below.
pub fn create_debug_cards(
    state: &mut GameState,
    request: DebugCardCreateRequest,
) -> Result<ActionResult, EngineError> {
    let debug_action = request.as_debug_action();
    preflight_debug_action(state, request.actor, &debug_action)?;
    if request.count == 0 {
        return Ok(ActionResult::applied(vec![], state.waiting_for.clone()));
    }
    let description = debug_action.describe(state);
    let before = state.clone();
    let DebugCardCreateRequest {
        actor,
        source,
        owner,
        zone,
        count,
        attach_to,
        run_etb,
        nonlegendary,
        creation_kind,
    } = request;
    let mut events = Vec::new();

    let mut result = if zone != Zone::Battlefield || !run_etb {
        for _ in 0..count {
            let initial_zone = if zone == Zone::Battlefield {
                Zone::Hand
            } else {
                zone
            };
            let object_id = materialize_debug_card(
                state,
                &source,
                owner,
                if zone == Zone::Battlefield {
                    attach_to
                } else {
                    None
                },
                nonlegendary,
                creation_kind,
                initial_zone,
            );
            if zone == Zone::Battlefield {
                let entry = route_debug_create_to_battlefield(state, object_id, false, None);
                events.extend(entry.events);
            }
        }
        ActionResult::applied(events, state.waiting_for.clone())
    } else {
        drain_debug_card_entries(
            state,
            PendingDebugCardEntries {
                source,
                owner,
                attach_to,
                nonlegendary,
                creation_kind,
                remaining: count,
            },
            &mut events,
        );
        ActionResult::applied(events, state.waiting_for.clone())
    };
    result.events.push(GameEvent::DebugActionUsed {
        player_id: actor,
        description,
    });
    result.log_entries = super::log::resolve_log_entries(&result.events, &before, state);
    Ok(result)
}

/// Viewer-safe form of [`create_debug_cards`] for transport boundaries.
///
/// The card source is already bound by the transport. Only the engine's
/// action-shaped refusal crosses this boundary; source lookup remains an
/// operational transport failure.
pub fn create_debug_cards_with_rejection(
    state: &mut GameState,
    request: DebugCardCreateRequest,
) -> Result<ActionResult, ActionRejection> {
    let actor = request.actor;
    let related_object_ids = GameAction::Debug(request.as_debug_action()).related_object_ids();
    if let Some(rejection) =
        explicit_debug_permission_rejection(state, actor, related_object_ids.clone())
    {
        return Err(rejection);
    }
    create_debug_cards(state, request).map_err(|error| {
        filter_action_rejection_for_viewer(
            state,
            actor,
            &action_rejection_for_engine_error(&error, related_object_ids),
        )
    })
}

/// Resume the active real-entry debug batch after its exact replacement or
/// as-enters child has completed.
pub(crate) fn drain_pending_debug_card_entries(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // A non-Priority state belongs to the entry's still-active child. Leave
    // the parent frame structurally intact until that child has settled.
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        return;
    }
    let Some(pending) = state
        .take_active_debug_card_entries()
        .expect("debug-card resumer may consume only its active frame")
    else {
        return;
    };
    drain_debug_card_entries(state, pending, events);
}

fn drain_debug_card_entries(
    state: &mut GameState,
    mut pending: PendingDebugCardEntries,
    events: &mut Vec<GameEvent>,
) {
    while pending.remaining > 0 && matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        let child_stack_start = state.resolution_stack.capture_child_boundary();
        let object_id = materialize_debug_card(
            state,
            &pending.source,
            pending.owner,
            None,
            pending.nonlegendary,
            pending.creation_kind,
            Zone::Hand,
        );
        pending.remaining -= 1;
        let entry = route_debug_create_to_battlefield(state, object_id, true, pending.attach_to);
        events.extend(entry.events);
        state.waiting_for = entry.waiting_for;

        if !matches!(state.waiting_for, WaitingFor::Priority { .. })
            || state.resolution_stack.capture_child_boundary() > child_stack_start
        {
            if state.resolution_stack.capture_child_boundary() > child_stack_start {
                state
                    .insert_debug_card_entries_parent_at_child_boundary(pending, child_stack_start)
                    .expect("debug-card parent must sit below the entry child stack");
            } else {
                state.push_debug_card_entries(pending);
            }
            return;
        }
    }
}

fn materialize_debug_card(
    state: &mut GameState,
    source: &DebugCardEntrySource,
    owner: PlayerId,
    attach_to: Option<AttachTarget>,
    nonlegendary: bool,
    creation_kind: DebugCardCreationKind,
    initial_zone: Zone,
) -> ObjectId {
    // CR 701.42a: a card entering mid-game can reach the same outside-the-game
    // faces (its meld pair's combined back) as one that started in the game.
    super::printed_cards::extend_card_face_registry(state, &source.outside_game_faces);
    // CR 400.7: The object receives an identity only at the point its own
    // entry starts; unattempted batch members are not game objects yet.
    let card_id = CardId(state.next_object_id);
    let object_id = zones::create_object(
        state,
        card_id,
        owner,
        source.face.name.clone(),
        initial_zone,
    );
    let object = state
        .objects
        .get_mut(&object_id)
        .expect("just-created debug card");
    super::printed_cards::apply_card_face_to_object(object, &source.face);
    object.back_face = source.back_face.clone();
    // CR 111.3 + CR 111.7: the sandbox may define a token from the printed
    // characteristics above; shared zone/SBA paths enforce token disappearance.
    object.is_token = match creation_kind {
        DebugCardCreationKind::Card => false,
        DebugCardCreationKind::Token => true,
    };
    // CR 205.4a-b: The sandbox override removes only the legendary
    // supertype from both copiable and current characteristics.
    if nonlegendary {
        object
            .base_card_types
            .supertypes
            .retain(|supertype| *supertype != Supertype::Legendary);
        object
            .card_types
            .supertypes
            .retain(|supertype| *supertype != Supertype::Legendary);
    }
    state.layers_dirty.mark_full();

    if let Some(target) = attach_to {
        match target {
            AttachTarget::Object(target_id) if state.objects.contains_key(&target_id) => {
                attach_object_to(state, object_id, target_id);
            }
            AttachTarget::Player(player_id)
                if state.players.iter().any(|player| player.id == player_id) =>
            {
                attach_to_player(state, object_id, player_id);
            }
            AttachTarget::Object(_) | AttachTarget::Player(_) => {}
        }
    }
    object_id
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
    use crate::game::{apply_as_current, filter_state_for_viewer};
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ReplacementDefinition, ReplacementMode,
    };
    use crate::types::actions::GameAction;
    use crate::types::card::LayoutKind;
    use crate::types::definitions::Definitions;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::PersistedGameState;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost};
    use crate::types::proposed_event::TokenCharacteristics;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::CoreType;

    fn sandbox_state() -> GameState {
        let mut state = GameState::new(FormatConfig::standard().with_sandbox(), 2, 42);
        state.debug_mode = true;
        state
    }

    #[test]
    fn debug_create_card_preflight_validates_owner_and_real_entry_context() {
        let mut state = sandbox_state();
        state.waiting_for = WaitingFor::GameOver { winner: None };

        let invalid_owner = DebugAction::CreateCard {
            card_name: "Debug Creature".into(),
            owner: PlayerId(9),
            zone: Zone::Hand,
            count: 1,
            attach_to: None,
            run_etb: true,
            nonlegendary: false,
            creation_kind: DebugCardCreationKind::Card,
        };
        let owner_error = preflight_debug_action(&state, PlayerId(0), &invalid_owner)
            .expect_err("CreateCard must name an existing owner");
        assert!(owner_error.to_string().contains("invalid owner player id"));

        let real_entry = DebugAction::CreateCard {
            card_name: "Debug Creature".into(),
            owner: PlayerId(0),
            zone: Zone::Battlefield,
            count: 1,
            attach_to: None,
            run_etb: true,
            nonlegendary: false,
            creation_kind: DebugCardCreationKind::Card,
        };
        let priority_error = preflight_debug_action(&state, PlayerId(0), &real_entry)
            .expect_err("a real battlefield entry may start only from Priority");
        assert!(priority_error.to_string().contains("Priority window"));

        let zero_entry = DebugAction::CreateCard {
            card_name: "Debug Creature".into(),
            owner: PlayerId(0),
            zone: Zone::Battlefield,
            count: 0,
            attach_to: None,
            run_etb: true,
            nonlegendary: false,
            creation_kind: DebugCardCreationKind::Card,
        };
        preflight_debug_action(&state, PlayerId(0), &zero_entry)
            .expect("zero is a no-op even off Priority");
        let hand_create = DebugAction::CreateCard {
            card_name: "Debug Creature".into(),
            owner: PlayerId(0),
            zone: Zone::Hand,
            count: 1,
            attach_to: None,
            run_etb: true,
            nonlegendary: false,
            creation_kind: DebugCardCreationKind::Card,
        };
        preflight_debug_action(&state, PlayerId(0), &hand_create)
            .expect("off-battlefield creation is synchronous off Priority");
        let token_in_hand = DebugAction::CreateCard {
            card_name: "Debug Creature".into(),
            owner: PlayerId(0),
            zone: Zone::Hand,
            count: 1,
            attach_to: None,
            run_etb: true,
            nonlegendary: false,
            creation_kind: DebugCardCreationKind::Token,
        };
        let token_zone_error = preflight_debug_action(&state, PlayerId(0), &token_in_hand)
            .expect_err("a debug card-token cannot be created outside the battlefield");
        assert!(token_zone_error
            .to_string()
            .contains("must be created on the battlefield"));
        let raw_battlefield_create = DebugAction::CreateCard {
            card_name: "Debug Creature".into(),
            owner: PlayerId(0),
            zone: Zone::Battlefield,
            count: 1,
            attach_to: None,
            run_etb: false,
            nonlegendary: false,
            creation_kind: DebugCardCreationKind::Card,
        };
        preflight_debug_action(&state, PlayerId(0), &raw_battlefield_create)
            .expect("raw battlefield creation is synchronous off Priority");
    }

    #[test]
    fn source_bound_debug_create_preflight_fails_before_materialization() {
        let mut state = sandbox_state();
        let revision = state.state_revision;
        let error = create_debug_cards(
            &mut state,
            DebugCardCreateRequest {
                actor: PlayerId(0),
                source: DebugCardEntrySource {
                    face: CardFace {
                        name: "Unmaterialized Debug Card".into(),
                        ..Default::default()
                    },
                    back_face: None,
                    outside_game_faces: Default::default(),
                },
                owner: PlayerId(9),
                zone: Zone::Hand,
                count: 1,
                attach_to: None,
                run_etb: true,
                nonlegendary: false,
                creation_kind: DebugCardCreationKind::Card,
            },
        )
        .expect_err("the source-bound creator must reuse the shared owner preflight");

        assert!(error.to_string().contains("invalid owner player id"));
        assert!(state.objects.is_empty());
        assert_eq!(state.state_revision, revision);

        state.debug_permitted.insert(PlayerId(0));
        let permission_error = create_debug_cards(
            &mut state,
            DebugCardCreateRequest {
                actor: PlayerId(1),
                source: DebugCardEntrySource {
                    face: CardFace {
                        name: "Unauthorized Debug Card".into(),
                        ..Default::default()
                    },
                    back_face: None,
                    outside_game_faces: Default::default(),
                },
                owner: PlayerId(0),
                zone: Zone::Hand,
                count: 1,
                attach_to: None,
                run_etb: true,
                nonlegendary: false,
                creation_kind: DebugCardCreationKind::Card,
            },
        )
        .expect_err("the actor carried by the source-bound request must be authorized");
        assert!(permission_error.to_string().contains("debug permission"));
        assert!(state.objects.is_empty());
        assert_eq!(state.state_revision, revision);
    }

    #[test]
    fn zero_debug_create_card_uses_the_shared_owner_preflight() {
        let mut state = sandbox_state();
        let revision = state.state_revision;
        let error = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::CreateCard {
                card_name: "No Card Needed".into(),
                owner: PlayerId(9),
                zone: Zone::Battlefield,
                count: 0,
                attach_to: None,
                run_etb: true,
                nonlegendary: false,
                creation_kind: DebugCardCreationKind::Card,
            }),
        )
        .expect_err("the action-boundary zero fast path must validate CreateCard owner");

        assert!(error.to_string().contains("invalid owner player id"));
        assert_eq!(state.state_revision, revision);
        assert!(state.objects.is_empty());
    }

    #[test]
    fn debug_create_card_batch_enters_battlefield_serially() {
        let mut state = sandbox_state();
        let source = DebugCardEntrySource {
            face: CardFace {
                name: "Debug Batch Creature".into(),
                ..Default::default()
            },
            back_face: None,
            outside_game_faces: Default::default(),
        };

        let result = create_debug_cards(
            &mut state,
            DebugCardCreateRequest {
                actor: PlayerId(0),
                source,
                owner: PlayerId(0),
                zone: Zone::Battlefield,
                count: 2,
                attach_to: None,
                run_etb: true,
                nonlegendary: false,
                creation_kind: DebugCardCreationKind::Token,
            },
        )
        .expect("an authorized debug batch should succeed");

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.resolution_stack.is_empty());
        assert_eq!(
            state
                .objects
                .values()
                .filter(|object| {
                    object.name == "Debug Batch Creature" && object.zone == Zone::Battlefield
                })
                .count(),
            2
        );
        assert!(state
            .objects
            .values()
            .filter(|object| object.name == "Debug Batch Creature")
            .all(|object| object.is_token));
    }

    #[test]
    fn debug_card_entry_batch_persists_its_unmaterialized_source() {
        let mut state = sandbox_state();
        state.push_debug_card_entries(PendingDebugCardEntries {
            source: DebugCardEntrySource {
                face: CardFace {
                    name: "Persisted Debug Card".into(),
                    ..Default::default()
                },
                back_face: None,
                outside_game_faces: Default::default(),
            },
            owner: PlayerId(0),
            attach_to: None,
            nonlegendary: false,
            creation_kind: DebugCardCreationKind::Card,
            remaining: 1,
        });

        let serialized = serde_json::to_string(&state).expect("debug batch should serialize");
        let restored: GameState =
            serde_json::from_str(&serialized).expect("debug batch should deserialize");
        let pending = restored
            .active_debug_card_entries()
            .expect("serialized debug batch should remain active");
        assert_eq!(pending.remaining, 1);
        assert_eq!(pending.source.face.name, "Persisted Debug Card");
        assert!(restored
            .objects
            .values()
            .all(|object| object.name != "Persisted Debug Card"));
    }

    /// CR 400.7 + CR 614.1 + CR 616.1: A sandbox batch may pause while each
    /// card enters. Only the active entrant is materialized; the later member
    /// stays in the private resolution frame across persistence, then enters
    /// exactly once after the replacement choice resolves.
    #[test]
    fn debug_card_entry_batch_resumes_after_persisted_replacement_choice() {
        let mut state = sandbox_state();
        let replacement_host = create_object(
            &mut state,
            CardId(900),
            PlayerId(1),
            "Debug entry replacement".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&replacement_host)
            .expect("replacement host exists")
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .mode(ReplacementMode::Optional { decline: None })
                    .description("Debug entry replacement".into()),
            );

        let result = create_debug_cards(
            &mut state,
            DebugCardCreateRequest {
                actor: PlayerId(0),
                source: DebugCardEntrySource {
                    face: CardFace {
                        name: "Paused Debug Batch Creature".into(),
                        ..Default::default()
                    },
                    back_face: None,
                    outside_game_faces: Default::default(),
                },
                owner: PlayerId(0),
                zone: Zone::Battlefield,
                count: 2,
                attach_to: None,
                run_etb: true,
                nonlegendary: false,
                creation_kind: DebugCardCreationKind::Card,
            },
        )
        .expect("an authorized debug batch should start");

        assert!(matches!(
            result.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert_eq!(
            result
                .events
                .iter()
                .filter(|event| matches!(event, GameEvent::DebugActionUsed { .. }))
                .count(),
            1,
            "the source-bound action emits one audit event when the batch starts"
        );
        assert_eq!(
            result.log_entries.len(),
            1,
            "the source-bound engine path resolves its audit log entry"
        );
        assert_eq!(
            state
                .objects
                .values()
                .filter(|object| object.name == "Paused Debug Batch Creature")
                .count(),
            1,
            "only the entrant that is waiting on a replacement choice is materialized"
        );
        assert_eq!(
            state
                .active_debug_card_entries()
                .expect("the remaining batch member is parked")
                .remaining,
            1
        );
        assert!(
            filter_state_for_viewer(&state, PlayerId(1))
                .resolution_stack
                .is_empty(),
            "the private source/frame never crosses a viewer-state boundary"
        );
        let pending_before = state
            .active_debug_card_entries()
            .cloned()
            .expect("the remaining batch member is active");
        let mut premature_events = Vec::new();
        drain_pending_debug_card_entries(&mut state, &mut premature_events);
        assert_eq!(
            state.active_debug_card_entries(),
            Some(&pending_before),
            "an off-Priority resume attempt must not consume the batch frame"
        );
        assert!(premature_events.is_empty());

        let persisted = PersistedGameState::capture(state);
        let serialized = serde_json::to_string(&persisted).expect("paused batch serializes");
        let persisted: PersistedGameState =
            serde_json::from_str(&serialized).expect("paused batch deserializes");
        let mut restored = persisted
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract");
        let first_resume =
            apply_as_current(&mut restored, GameAction::ChooseReplacement { index: 0 })
                .expect("replacement choice resumes the serial batch");
        assert!(first_resume
            .events
            .iter()
            .all(|event| !matches!(event, GameEvent::DebugActionUsed { .. })));

        assert!(matches!(
            restored.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        let second_resume =
            apply_as_current(&mut restored, GameAction::ChooseReplacement { index: 0 })
                .expect("the remaining entrant presents and resumes its own replacement choice");
        assert!(second_resume
            .events
            .iter()
            .all(|event| !matches!(event, GameEvent::DebugActionUsed { .. })));

        assert!(matches!(restored.waiting_for, WaitingFor::Priority { .. }));
        assert!(restored.resolution_stack.is_empty());
        assert_eq!(
            restored
                .objects
                .values()
                .filter(|object| {
                    object.name == "Paused Debug Batch Creature" && object.zone == Zone::Battlefield
                })
                .count(),
            2,
            "the resumed entry and the single remaining batch member each enter once"
        );
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
            .units()
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
            loyalty: None,
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
            is_swap_snapshot: false,
            trigger_printed_origins: Vec::new(),
            name: "Test Prepare Face".to_string(),
            power: None,
            toughness: None,
            loyalty: None,
            printed_loyalty: None,
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
            parse_warnings: vec![],
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
            count: 1,
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

    /// CR 111.4 + CR 614.1a: a debug preset describes the requested token only.
    /// Chatterfang's replacement adds a Squirrel batch to the same CreateToken
    /// event; the preset's image ref and catalog abilities are stamped on the
    /// Pest alone, and the Squirrel keeps its own identity (Chatterfang ruling:
    /// the Squirrels "won't have any abilities the other tokens were created
    /// with").
    #[test]
    fn debug_preset_token_stamps_only_the_requested_batch() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::types::phase::Phase;

        const CHATTERFANG: &str = "Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\nIf one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.\n{B}, Sacrifice X Squirrels: Target creature gets +X/-X until end of turn.";

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
        let mut runner = scenario.build();
        runner.state_mut().format_config = FormatConfig::standard().with_sandbox();
        runner.state_mut().debug_mode = true;

        let sos_pest_preset_id = "00a0801d-0212-5890-8957-3cde30f382f9";
        let action = GameAction::Debug(DebugAction::CreateToken {
            request: DebugTokenRequest::Preset {
                preset_id: sos_pest_preset_id.to_string(),
                owner: PlayerId(0),
                power_override: None,
                toughness_override: None,
                enter_with_counters: Vec::new(),
            },
            count: 1,
            run_etb: true,
        });
        crate::game::engine::apply(runner.state_mut(), PlayerId(0), action)
            .expect("debug CreateToken preset should succeed");

        let state = runner.state();
        let token_with_subtype = |subtype: &str| {
            state
                .battlefield
                .iter()
                .filter_map(|id| state.objects.get(id))
                .filter(|obj| obj.is_token && obj.card_types.subtypes.iter().any(|s| s == subtype))
                .collect::<Vec<_>>()
        };
        let pests = token_with_subtype("Pest");
        let squirrels = token_with_subtype("Squirrel");
        assert_eq!(pests.len(), 1, "the requested Pest is created");
        // Reach guard: Chatterfang's appended batch was created.
        assert_eq!(squirrels.len(), 1, "Chatterfang adds one Squirrel");
        let (pest, squirrel) = (pests[0], squirrels[0]);
        assert!(
            pest.token_image_ref.is_some(),
            "the Pest carries its preset"
        );
        assert_eq!(
            pest.trigger_definitions.len(),
            1,
            "the Pest's catalog trigger"
        );
        assert_ne!(
            squirrel.token_image_ref, pest.token_image_ref,
            "the Squirrel is not stamped with the Pest preset"
        );
        assert!(
            squirrel.trigger_definitions.is_empty(),
            "the Squirrel gains none of the Pest's abilities"
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
            count: 1,
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
            count: 1,
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
            count: 1,
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
            count: 1,
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
    fn debug_create_token_batch_uses_one_replacement_event() {
        let mut state = sandbox_state();
        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::CreateToken {
                request: DebugTokenRequest::Custom {
                    owner: PlayerId(0),
                    characteristics: zero_zero_creature(),
                    enter_with_counters: vec![(CounterType::Plus1Plus1, 1)],
                },
                count: 2,
                run_etb: true,
            }),
        )
        .expect("a two-token debug batch should use the normal token pipeline");

        assert_eq!(
            result
                .events
                .iter()
                .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
                .count(),
            2,
            "the count must reach the single CreateToken replacement event"
        );
    }

    #[test]
    fn debug_create_zero_is_authorized_noop_without_finalization() {
        let mut state = sandbox_state();
        let revision = state.state_revision;
        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::CreateToken {
                request: DebugTokenRequest::Custom {
                    owner: PlayerId(0),
                    characteristics: zero_zero_creature(),
                    enter_with_counters: Vec::new(),
                },
                count: 0,
                run_etb: true,
            }),
        )
        .expect("an authorized zero-count create must be a no-op");

        assert_eq!(state.state_revision, revision);
        assert!(state.objects.is_empty());
        assert!(result.events.is_empty());
        assert!(result.log_entries.is_empty());
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
                count: 2,
                nonlegendary: false,
            }),
        )
        .expect("debug CreateTokenCopy should succeed");

        assert_eq!(
            result
                .events
                .iter()
                .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
                .count(),
            2,
            "copy count must reach the existing CopyTokenOf resolver"
        );

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
    fn debug_create_token_copy_can_strip_legendary() {
        let mut state = sandbox_state();
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Legendary Copy Source".to_string(),
            Zone::Battlefield,
        );
        let source = state.objects.get_mut(&source_id).unwrap();
        source.base_card_types.supertypes.push(Supertype::Legendary);
        source.card_types.supertypes.push(Supertype::Legendary);

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::CreateTokenCopy {
                source_id,
                owner: PlayerId(0),
                count: 1,
                nonlegendary: true,
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
        let token = &state.objects[&token_id];

        assert!(!token.card_types.supertypes.contains(&Supertype::Legendary));
        assert!(!token
            .base_card_types
            .supertypes
            .contains(&Supertype::Legendary));
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

    /// Phase D review fix: a `DebugCommand` zone change skips the delivery
    /// tail, not just the replacement consult (it installs only a planeswalker's
    /// or battle's CR 306.5b / CR 310.4b counters on arrival). Pending ETB
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
    fn debug_counter_decrement_handles_i32_min_without_overflow() {
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
            .insert(CounterType::Generic("test".to_string()), 1);

        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::ModifyCounters {
                object_id,
                counter_type: CounterType::Generic("test".to_string()),
                delta: i32::MIN,
            }),
        )
        .expect("the largest representable decrement must saturate safely");

        assert!(state.objects[&object_id].counters.is_empty());
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
            count: 1,
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

    /// CR 614.1c + CR 603.6a: `MoveToZone { simulate: true }` onto the
    /// battlefield runs the real entry pipeline — the "enters tapped"
    /// replacement applies and the ETB trigger fires. `simulate: false` stages
    /// the permanent raw with neither.
    #[test]
    fn debug_move_to_battlefield_simulate_runs_etb_effects() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::types::phase::Phase;

        for simulate in [true, false] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let id = scenario
                .add_creature_to_hand_from_oracle(
                    P0,
                    "Entering Creature",
                    2,
                    2,
                    "This creature enters tapped.\nWhen this creature enters, you gain 3 life.",
                )
                .id();
            let mut runner = scenario.build();
            runner.state_mut().debug_mode = true;
            let life_before = runner.state().players[0].life;

            runner
                .act(GameAction::Debug(DebugAction::MoveToZone {
                    object_id: id,
                    to_zone: Zone::Battlefield,
                    library_position: None,
                    simulate,
                }))
                .expect("debug MoveToZone battlefield should succeed");
            runner.advance_until_stack_empty();

            let obj = &runner.state().objects[&id];
            assert_eq!(obj.zone, Zone::Battlefield);
            assert_eq!(obj.tapped, simulate, "simulate={simulate}: enters-tapped");
            let expected_life = life_before + if simulate { 3 } else { 0 };
            assert_eq!(
                runner.state().players[0].life,
                expected_life,
                "simulate={simulate}: ETB trigger"
            );
        }
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

    // ── Sandbox GainLife / LoseLife (CR 119.3 life-change event) ─────────────

    mod life_change {
        use super::*;
        use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
        use crate::game::scenario_db::GameScenarioDbExt;
        use crate::types::ability::{ChooseOneOfKind, PlayerFilter};
        use crate::types::actions::MAX_DEBUG_LIFE_AMOUNT;
        use crate::types::game_state::{PendingReplacement, UnrepresentablePolicy};
        use crate::types::phase::Phase;

        pub(super) fn scenario() -> GameScenario {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario
        }

        pub(super) fn add_card(
            scenario: &mut GameScenario,
            player: PlayerId,
            name: &str,
        ) -> ObjectId {
            scenario.add_real_card(
                player,
                name,
                Zone::Battlefield,
                crate::test_support::shared_card_db(),
            )
        }

        pub(super) fn debug_runner(scenario: GameScenario) -> GameRunner {
            let mut runner = scenario.build();
            runner.state_mut().debug_mode = true;
            runner
        }

        fn gain(player_id: PlayerId, amount: u32) -> GameAction {
            GameAction::Debug(DebugAction::GainLife { player_id, amount })
        }

        fn lose(player_id: PlayerId, amount: u32) -> GameAction {
            GameAction::Debug(DebugAction::LoseLife { player_id, amount })
        }

        /// Submits a debug action through `apply` and asserts the reach guard:
        /// the action got past preflight and the handler (`DebugActionUsed`).
        fn act_ok(runner: &mut GameRunner, action: GameAction) -> ActionResult {
            let result = runner.act(action).expect("debug life change must apply");
            assert!(
                result.events.iter().any(|event| matches!(
                    event,
                    GameEvent::DebugActionUsed { player_id, .. } if *player_id == P0
                )),
                "reach guard: the debug action must be audited as used by P0"
            );
            result
        }

        fn life(runner: &GameRunner, player: PlayerId) -> i32 {
            runner.life(player)
        }

        pub(super) fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
            runner.state().objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        }

        fn in_graveyard(runner: &GameRunner, player: PlayerId, id: ObjectId) -> bool {
            runner.state().players[player.0 as usize]
                .graveyard
                .contains(&id)
        }

        /// The typed rejection of an unrepresentable final life edit.
        fn assert_not_representable(error: &EngineError) {
            assert!(
                matches!(error, EngineError::LifeChangeUnrepresentable { .. }),
                "expected the typed unrepresentable-life rejection, got {error:?}"
            );
        }

        /// Everything the action boundary must restore exactly when it rejects
        /// an action: every player's life and per-turn counters, the state
        /// revision, the prompt and its parked proposal, the stack, the resolved
        /// rules journal and the Pridemate's counters.
        #[derive(Debug, PartialEq)]
        struct RollbackSet {
            players: Vec<(i32, u32, u32)>,
            state_revision: u64,
            waiting_for: WaitingFor,
            pending_replacement: Option<PendingReplacement>,
            stack_is_empty: bool,
            journal_len: usize,
            pridemate_counters: u32,
        }

        fn rollback_set(runner: &GameRunner, pridemate: ObjectId) -> RollbackSet {
            let state = runner.state();
            RollbackSet {
                players: state
                    .players
                    .iter()
                    .map(|player| {
                        (
                            player.life,
                            player.life_gained_this_turn,
                            player.life_lost_this_turn,
                        )
                    })
                    .collect(),
                state_revision: state.state_revision,
                waiting_for: state.waiting_for.clone(),
                pending_replacement: state.pending_replacement.clone(),
                stack_is_empty: state.stack.is_empty(),
                journal_len: state.resolved_rules_journal.entries().len(),
                pridemate_counters: p1p1(runner, pridemate),
            }
        }

        pub(super) fn replacement_prompt_open(runner: &GameRunner) -> bool {
            matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            )
        }

        pub(super) fn parked_policy(runner: &GameRunner) -> Option<UnrepresentablePolicy> {
            runner
                .state()
                .pending_replacement
                .as_ref()
                .map(|pending| pending.unrepresentable_life_change)
        }

        /// Answers the open replacement-order prompts with index 0 until one
        /// answer is rejected; returns the number of answers accepted before it,
        /// the rollback set taken just before the rejected answer, and its error.
        fn answer_until_rejected(
            runner: &mut GameRunner,
            pridemate: ObjectId,
        ) -> (usize, RollbackSet, EngineError) {
            for accepted in 0..16 {
                assert!(
                    replacement_prompt_open(runner),
                    "reach guard: an ordering prompt must be open, got {:?}",
                    runner.state().waiting_for
                );
                let before = rollback_set(runner, pridemate);
                match runner.act(GameAction::ChooseReplacement { index: 0 }) {
                    Ok(_) => {}
                    Err(error) => return (accepted, before, error),
                }
            }
            panic!("no replacement answer was rejected");
        }

        /// The delivering `ChooseReplacement` is rejected with the exact error,
        /// the previously accepted proposal and prompt are restored, and
        /// answering again is rejected the same way (the prompt stays answerable).
        fn assert_choice_rejected_and_still_answerable(
            runner: &mut GameRunner,
            pridemate: ObjectId,
        ) -> usize {
            let (accepted, before, error) = answer_until_rejected(runner, pridemate);
            assert_not_representable(&error);
            assert_eq!(
                rollback_set(runner, pridemate),
                before,
                "a rejected answer must restore the accepted proposal and prompt"
            );
            assert!(replacement_prompt_open(runner));
            let again = runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect_err("the restored prompt must reject the same answer again");
            assert_not_representable(&again);
            assert_eq!(rollback_set(runner, pridemate), before);
            accepted
        }

        /// Answers any open ordering prompts with index 0 until none remains.
        fn answer_to_completion(runner: &mut GameRunner) {
            for _ in 0..16 {
                if !replacement_prompt_open(runner) {
                    return;
                }
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("a representable control answer must be accepted");
            }
            panic!("the replacement prompts never completed");
        }

        /// Submits `action` expecting an unrepresentable final result: either the
        /// action itself is rejected with the rollback set unchanged, or it parks
        /// an ordering prompt whose delivering answer is.
        fn submit_expecting_rejection(
            runner: &mut GameRunner,
            pridemate: ObjectId,
            action: GameAction,
        ) {
            let before = rollback_set(runner, pridemate);
            match runner.act(action) {
                Err(error) => {
                    assert_not_representable(&error);
                    assert_eq!(
                        rollback_set(runner, pridemate),
                        before,
                        "the rejected action must leave the state unchanged"
                    );
                }
                Ok(_) => {
                    assert_choice_rejected_and_still_answerable(runner, pridemate);
                }
            }
        }

        /// Puts `count` Bloodletter of Aclazotz objects (P0) in play, with the
        /// replacement parsed from the card's verbatim Oracle text.
        fn add_bloodletters(runner: &mut GameRunner, count: usize) {
            let parsed = crate::parser::oracle::parse_oracle_text(
                "Flying\nIf an opponent would lose life during your turn, they lose twice that much \
                 life instead. (Damage causes loss of life.)",
                "Bloodletter of Aclazotz",
                &["Flying".to_string()],
                &["Creature".to_string()],
                &["Vampire".to_string(), "Demon".to_string()],
            );
            assert_eq!(
                parsed.replacements.len(),
                1,
                "reach guard: Bloodletter's life-loss doubler must parse"
            );
            for index in 0..count {
                let id = create_object(
                    runner.state_mut(),
                    CardId(9_800 + index as u64),
                    P0,
                    "Bloodletter of Aclazotz".to_string(),
                    Zone::Battlefield,
                );
                let object = runner.state_mut().objects.get_mut(&id).unwrap();
                object.replacement_definitions = parsed.replacements.clone().into();
                object.base_replacement_definitions =
                    std::sync::Arc::new(parsed.replacements.clone());
            }
        }

        /// CR 119.3 + CR 603.2 + CR 603.3: a debug gain is a real life-gain event,
        /// so the recipient's "whenever you gain life" trigger goes on the stack
        /// and the other player's does not.
        #[test]
        fn debug_gain_life_fires_recipient_life_gain_trigger() {
            let mut scenario = scenario();
            let p0_mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            let p1_mate = add_card(&mut scenario, P1, "Ajani's Pridemate");
            let mut runner = debug_runner(scenario);

            act_ok(&mut runner, gain(P0, 3));
            assert_eq!(life(&runner, P0), 23);
            assert_eq!(
                runner.state().stack.len(),
                1,
                "exactly P0's Pridemate trigger must be on the stack after the action"
            );
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player } if player == P0
            ));

            runner.advance_until_stack_empty();
            assert_eq!(p1p1(&runner, p0_mate), 1);
            assert_eq!(p1p1(&runner, p1_mate), 0);
        }

        /// The recipient is the payload's player, never the actor implicitly.
        #[test]
        fn debug_gain_life_targets_payload_player_not_actor() {
            let mut scenario = scenario();
            let p0_mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            let p1_mate = add_card(&mut scenario, P1, "Ajani's Pridemate");
            let mut runner = debug_runner(scenario);

            act_ok(&mut runner, gain(P1, 3));
            assert_eq!(life(&runner, P1), 23);
            assert_eq!(life(&runner, P0), 20);
            runner.advance_until_stack_empty();
            assert_eq!(p1p1(&runner, p1_mate), 1);
            assert_eq!(p1p1(&runner, p0_mate), 0);
        }

        /// CR 101.2: "can't gain life" beats the gain and any doubling replacement.
        #[test]
        fn debug_gain_life_respects_cant_gain_static() {
            for with_doubler in [false, true] {
                let mut scenario = scenario();
                let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
                add_card(&mut scenario, P0, "Forsaken Wastes");
                if with_doubler {
                    add_card(&mut scenario, P0, "Rhox Faithmender");
                }
                let mut runner = debug_runner(scenario);

                act_ok(&mut runner, gain(P0, 3));
                assert_eq!(life(&runner, P0), 20, "with_doubler={with_doubler}");
                assert!(
                    runner.state().stack.is_empty(),
                    "with_doubler={with_doubler}"
                );
                runner.advance_until_stack_empty();
                assert_eq!(p1p1(&runner, mate), 0, "with_doubler={with_doubler}");
            }
        }

        /// CR 119.10 + CR 614.1a: a "would gain life … instead" replacement
        /// rewrites the debug gain, and the modified event triggers once.
        #[test]
        fn debug_gain_life_applies_life_gain_replacement() {
            let mut scenario = scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Rhox Faithmender");
            let mut runner = debug_runner(scenario);

            act_ok(&mut runner, gain(P0, 3));
            assert_eq!(life(&runner, P0), 26);
            runner.advance_until_stack_empty();
            assert_eq!(p1p1(&runner, mate), 1);
        }

        /// CR 119.9: gaining 0 life is no life-gain event and triggers nothing.
        #[test]
        fn debug_gain_zero_life_is_not_a_life_gain_event() {
            let mut scenario = scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            let mut runner = debug_runner(scenario);

            act_ok(&mut runner, gain(P0, 0));
            assert_eq!(life(&runner, P0), 20);
            assert!(runner.state().stack.is_empty());
            runner.advance_until_stack_empty();
            assert_eq!(p1p1(&runner, mate), 0);
        }

        /// CR 603.7: a delayed "whenever a player gains life" trigger created by a
        /// resolved spell sees the debug gain.
        #[test]
        fn debug_gain_life_fires_delayed_life_gain_trigger() {
            let mut scenario = scenario();
            let false_cure = scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "False Cure",
                    true,
                    "Until end of turn, whenever a player gains life, that player loses 2 life for each 1 life they gained.",
                )
                .with_mana_cost(ManaCost::zero())
                .id();
            let mut runner = debug_runner(scenario);
            runner.cast(false_cure).resolve();
            runner.advance_until_stack_empty();
            assert_eq!(
                life(&runner, P0),
                20,
                "sanity: False Cure itself changes no life"
            );

            act_ok(&mut runner, gain(P0, 2));
            runner.advance_until_stack_empty();
            assert_eq!(life(&runner, P0), 18, "20 + 2 - (2 × 2)");
        }

        /// CR 119.3 + CR 603.2: a debug loss is a real life-loss event, so an
        /// opponent's "whenever an opponent loses life" trigger fires; the
        /// controller's own loss does not trigger it.
        #[test]
        fn debug_lose_life_fires_life_loss_trigger() {
            let mut scenario = scenario();
            add_card(&mut scenario, P1, "Exquisite Blood");
            let mut runner = debug_runner(scenario);

            act_ok(&mut runner, lose(P0, 3));
            assert_eq!(life(&runner, P0), 17);
            runner.advance_until_stack_empty();
            assert_eq!(life(&runner, P1), 23);

            let mut scenario = self::scenario();
            add_card(&mut scenario, P1, "Exquisite Blood");
            let mut runner = debug_runner(scenario);
            act_ok(&mut runner, lose(P1, 3));
            assert_eq!(life(&runner, P1), 17);
            assert!(runner.state().stack.is_empty());
            runner.advance_until_stack_empty();
            assert_eq!(life(&runner, P0), 20);
        }

        /// CR 119.6 + CR 704.5a: a player brought to 0 life loses the game.
        #[test]
        fn debug_lose_life_to_zero_loses_the_game() {
            let mut scenario = scenario();
            scenario.with_life(P0, 3);
            let mut runner = debug_runner(scenario);

            act_ok(&mut runner, lose(P0, 3));
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::GameOver { .. }
            ));
        }

        /// CR 101.2: "your life total can't change" stops the debug loss for that
        /// player only.
        #[test]
        fn debug_lose_life_respects_cant_lose_static() {
            let mut scenario = scenario();
            add_card(&mut scenario, P0, "Platinum Emperion");
            let mut runner = debug_runner(scenario);

            act_ok(&mut runner, lose(P0, 3));
            assert_eq!(life(&runner, P0), 20);
            act_ok(&mut runner, lose(P1, 3));
            assert_eq!(life(&runner, P1), 17);
        }

        fn death_shadow_scenario() -> (GameScenario, ObjectId) {
            let mut scenario = scenario();
            scenario.with_life(P0, 12);
            let shadow = add_card(&mut scenario, P0, "Death's Shadow");
            (scenario, shadow)
        }

        fn assert_shadow_toughness_one(runner: &mut GameRunner, shadow: ObjectId) {
            crate::game::layers::evaluate_layers(runner.state_mut());
            assert_eq!(
                runner.state().objects[&shadow].toughness,
                Some(1),
                "reach guard: Death's Shadow is 1 toughness at 12 life"
            );
        }

        /// CR 704.3 + CR 704.5f: the debug gain runs SBAs, so a creature whose
        /// toughness the new life total drops to 0 is put into the graveyard.
        #[test]
        fn debug_gain_life_runs_state_based_actions() {
            let (scenario, shadow) = death_shadow_scenario();
            let mut runner = debug_runner(scenario);
            assert_shadow_toughness_one(&mut runner, shadow);

            act_ok(&mut runner, gain(P0, 1));
            assert!(in_graveyard(&runner, P0, shadow));
        }

        /// CR 704.3 + CR 603.2 + CR 603.3: events the SBAs generate are scanned
        /// for triggers, so a dies trigger caused by the debug life change fires.
        #[test]
        fn debug_gain_life_scans_sba_dies_triggers() {
            let (mut scenario, shadow) = death_shadow_scenario();
            add_card(&mut scenario, P0, "Zulaport Cutthroat");
            let mut runner = debug_runner(scenario);
            assert_shadow_toughness_one(&mut runner, shadow);

            act_ok(&mut runner, gain(P0, 1));
            assert!(in_graveyard(&runner, P0, shadow));
            assert_eq!(life(&runner, P0), 13);
            assert_eq!(runner.state().stack.len(), 1, "Zulaport's dies trigger");

            runner.advance_until_stack_empty();
            assert_eq!(life(&runner, P1), 19);
            assert_eq!(life(&runner, P0), 14);
        }

        /// CR 603.3: a targeted dies trigger caused by the debug life change opens
        /// its target prompt.
        #[test]
        fn debug_gain_life_opens_sba_trigger_target_prompt() {
            let (mut scenario, shadow) = death_shadow_scenario();
            add_card(&mut scenario, P0, "Blood Artist");
            let mut runner = debug_runner(scenario);
            assert_shadow_toughness_one(&mut runner, shadow);

            let result = act_ok(&mut runner, gain(P0, 1));
            assert!(in_graveyard(&runner, P0, shadow));
            assert!(
                matches!(
                    result.waiting_for,
                    WaitingFor::TriggerTargetSelection { player, .. } if player == P0
                ),
                "Blood Artist's target prompt must open, got {:?}",
                result.waiting_for
            );

            runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Player(P1)],
                })
                .expect("Blood Artist target");
            runner.advance_until_stack_empty();
            assert_eq!(life(&runner, P1), 19);
            assert_eq!(life(&runner, P0), 14);
        }

        /// CR 616.1: with two applicable gain replacements the affected player
        /// orders them; nothing happens until the choice resolves.
        #[test]
        fn debug_gain_life_defers_to_replacement_order_choice() {
            let mut scenario = scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Boon Reflection");
            add_card(&mut scenario, P0, "Leyline of Hope");
            let mut runner = debug_runner(scenario);

            act_ok(&mut runner, gain(P0, 3));
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ),
                "got {:?}",
                runner.state().waiting_for
            );
            assert_eq!(life(&runner, P0), 20);
            assert!(runner.state().stack.is_empty());
            assert_eq!(p1p1(&runner, mate), 0);

            for _ in 0..4 {
                if !matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ) {
                    break;
                }
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("replacement order choice");
            }
            runner.advance_until_stack_empty();
            let final_life = life(&runner, P0);
            assert!(
                final_life == 27 || final_life == 28,
                "Boon first gives 3→6→7 (27); Leyline first gives 3→4→8 (28); got {final_life}"
            );
            assert_eq!(p1p1(&runner, mate), 1);
        }

        /// CR 616.1: while the first gain's replacement-order choice is open, a
        /// second debug life change is rejected before any state change, so the
        /// outstanding proposal stays resumable and completes exactly once.
        #[test]
        fn debug_life_change_rejected_while_replacement_choice_pending() {
            let mut scenario = scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Boon Reflection");
            add_card(&mut scenario, P0, "Leyline of Hope");
            let mut runner = debug_runner(scenario);

            act_ok(&mut runner, gain(P0, 3));
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ),
                "reach guard: the first gain must park a replacement-order choice, got {:?}",
                runner.state().waiting_for
            );
            let pending_before = runner.state().pending_replacement.clone();
            assert!(
                pending_before.is_some(),
                "reach guard: the first gain's proposal must be pending"
            );
            let waiting_before = runner.state().waiting_for.clone();

            for second in [gain(P0, 5), lose(P0, 2)] {
                let error = runner
                    .act(second.clone())
                    .expect_err("a debug life change must not be admitted under an open prompt");
                assert!(
                    matches!(
                        &error,
                        EngineError::InvalidAction(message) if message.contains("Priority window")
                    ),
                    "{second:?} must be rejected by the Priority-boundary preflight, got {error:?}"
                );
                assert_eq!(runner.state().pending_replacement, pending_before);
                assert_eq!(runner.state().waiting_for, waiting_before);
                assert_eq!(life(&runner, P0), 20);
                assert!(runner.state().stack.is_empty());
                assert_eq!(p1p1(&runner, mate), 0);
            }

            for _ in 0..4 {
                if !matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ) {
                    break;
                }
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("the first gain's replacement order choice must stay resumable");
            }
            runner.advance_until_stack_empty();
            let final_life = life(&runner, P0);
            assert!(
                final_life == 27 || final_life == 28,
                "only the first gain applies: Boon first 3→6→7 (27), Leyline first 3→4→8 (28); got {final_life}"
            );
            assert_eq!(
                p1p1(&runner, mate),
                1,
                "the first gain must complete exactly once"
            );
        }

        /// CR 614.6: a substitute that pauses on its own prompt leaves the debug
        /// action applied with the substitute's prompt pending and nothing
        /// dispatched.
        #[test]
        fn debug_gain_life_pauses_on_interactive_substitution() {
            let mut scenario = scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            let mut runner = debug_runner(scenario);
            let lich = create_object(
                runner.state_mut(),
                CardId(9_702),
                P0,
                "Interactive Lich".to_string(),
                Zone::Battlefield,
            );
            let substitution = ReplacementDefinition::new(ReplacementEvent::GainLife).execute(
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChooseOneOf {
                        chooser: PlayerFilter::Controller,
                        branches: vec![AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::GainLife {
                                amount: QuantityExpr::Fixed { value: 1 },
                                player: TargetFilter::Controller,
                            },
                        )],
                        kind: ChooseOneOfKind::Ordinary,
                    },
                ),
            );
            {
                let obj = runner.state_mut().objects.get_mut(&lich).unwrap();
                obj.replacement_definitions = vec![substitution.clone()].into();
                obj.base_replacement_definitions = std::sync::Arc::new(vec![substitution]);
            }

            act_ok(&mut runner, gain(P0, 4));
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::ChooseOneOfBranch { .. }
                ),
                "got {:?}",
                runner.state().waiting_for
            );
            assert_eq!(life(&runner, P0), 20);
            assert!(runner.state().stack.is_empty());
            assert_eq!(p1p1(&runner, mate), 0);
        }

        /// CR 608.2c + CR 614.6: an unrepresentable branch answer restores the
        /// live prompt and interaction; a legal alternate branch may re-pause,
        /// then its nested choice and the replacement tail each resolve once.
        #[test]
        fn interactive_lich_branch_rejects_loss_overflow_then_resumes_valid_nested_choice() {
            let setup = |life_lost_this_turn| {
                let mut scenario = scenario();
                scenario.with_library_top(P0, &["Forest", "Island", "Mountain"]);
                let mut runner = debug_runner(scenario);
                runner.state_mut().players[0].life_lost_this_turn = life_lost_this_turn;
                let lich = create_object(
                    runner.state_mut(),
                    CardId(9_703),
                    P0,
                    "Interactive Lich".to_string(),
                    Zone::Battlefield,
                );
                let loss = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::LoseLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                        target: None,
                    },
                );
                let nested = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChooseOneOf {
                        chooser: PlayerFilter::Controller,
                        branches: vec![AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::Draw {
                                count: QuantityExpr::Fixed { value: 1 },
                                target: TargetFilter::Controller,
                            },
                        )],
                        kind: ChooseOneOfKind::Ordinary,
                    },
                );
                let tail = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                );
                let substitution = ReplacementDefinition::new(ReplacementEvent::GainLife).execute(
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChooseOneOf {
                            chooser: PlayerFilter::Controller,
                            branches: vec![loss, nested],
                            kind: ChooseOneOfKind::Ordinary,
                        },
                    )
                    .sub_ability(tail),
                );
                {
                    let obj = runner.state_mut().objects.get_mut(&lich).unwrap();
                    obj.replacement_definitions = vec![substitution.clone()].into();
                    obj.base_replacement_definitions = std::sync::Arc::new(vec![substitution]);
                }
                (runner, lich)
            };

            // Positive control: the loss branch completes with its tail once.
            let (mut control, control_lich) = setup(0);
            let control_hand = control.state().players[0].hand.len();
            act_ok(&mut control, gain(P0, 4));
            assert!(matches!(
                control.state().waiting_for,
                WaitingFor::ChooseOneOfBranch { .. }
            ));
            control
                .act(GameAction::ChooseBranch { index: 0 })
                .expect("the representable loss branch completes");
            assert_eq!(life(&control, P0), 18);
            assert_eq!(control.state().players[0].life_lost_this_turn, 2);
            assert_eq!(control.state().players[0].hand.len(), control_hand + 1);
            assert!(matches!(
                control.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert_eq!(
                control.state().objects[&control_lich]
                    .replacement_definitions
                    .len(),
                1
            );

            // Hostile path: a real life-loss counter overflow rejects the answer
            // without deleting the replacement source to force the outcome.
            let (mut runner, lich) = setup(u32::MAX - 1);
            let initial_hand = runner.state().players[0].hand.len();
            act_ok(&mut runner, gain(P0, 4));
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseOneOfBranch { .. }
            ));
            assert_eq!(
                runner.state().objects[&lich].replacement_definitions.len(),
                1
            );
            let prompt_entry = runner.state().clone();

            let error = runner
                .act(GameAction::ChooseBranch { index: 0 })
                .expect_err("the loss branch overflows the life-lost counter");
            assert_not_representable(&error);
            assert_eq!(
                runner.state(),
                &prompt_entry,
                "the rejection restores the prompt, interaction authority, replacement, and all game state"
            );
            assert_eq!(
                runner.state().objects[&lich].replacement_definitions.len(),
                1
            );

            runner
                .act(GameAction::ChooseBranch { index: 1 })
                .expect("the alternate branch remains available after rejection");
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::ChooseOneOfBranch { .. }
                ),
                "the valid alternate opens its nested choice"
            );
            assert_eq!(life(&runner, P0), 20);
            assert_eq!(runner.state().players[0].life_lost_this_turn, u32::MAX - 1);
            assert_eq!(runner.state().players[0].hand.len(), initial_hand);

            runner
                .act(GameAction::ChooseBranch { index: 0 })
                .expect("the nested draw resolves and drains the replacement tail");
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert_eq!(runner.state().players[0].hand.len(), initial_hand + 2);
            assert_eq!(life(&runner, P0), 20);
            assert_eq!(runner.state().players[0].life_lost_this_turn, u32::MAX - 1);
            assert!(!runner.state().has_post_replacement_drain());
            assert_eq!(
                runner.state().objects[&lich].replacement_definitions.len(),
                1
            );
        }

        /// Older parked replacement payloads omit the policy and deserialize
        /// as `IgnoreImpossible`; a resumed public action still rejects and
        /// restores on the ingress life window's recorded failure.
        #[test]
        fn legacy_pending_replacement_policy_still_rejects_public_overflow() {
            let mut scenario = scenario();
            scenario.with_life(P0, i32::MAX - 1);
            add_card(&mut scenario, P0, "Boon Reflection");
            add_card(&mut scenario, P0, "Leyline of Hope");
            let mut runner = debug_runner(scenario);

            act_ok(&mut runner, gain(P0, 3));
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
            let mut legacy_json = serde_json::to_value(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .expect("the ordering prompt owns a pending replacement"),
            )
            .expect("pending replacement serializes");
            legacy_json
                .as_object_mut()
                .expect("pending replacement serializes as an object")
                .remove("unrepresentable_life_change");
            runner.state_mut().pending_replacement = Some(
                serde_json::from_value::<PendingReplacement>(legacy_json)
                    .expect("old payload shape deserializes"),
            );
            assert_eq!(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .unrepresentable_life_change,
                UnrepresentablePolicy::IgnoreImpossible
            );

            let mut rejected = false;
            for _ in 0..4 {
                let prompt_entry = runner.state().clone();
                match runner.act(GameAction::ChooseReplacement { index: 0 }) {
                    Ok(_)
                        if matches!(
                            runner.state().waiting_for,
                            WaitingFor::ReplacementChoice { .. }
                        ) =>
                    {
                        continue
                    }
                    Ok(_) => panic!("the overflowed legacy gain must not complete"),
                    Err(error) => {
                        assert_not_representable(&error);
                        assert_eq!(runner.state(), &prompt_entry);
                        rejected = true;
                        break;
                    }
                }
            }
            assert!(
                rejected,
                "the old policy must not turn public overflow into success"
            );
        }

        /// The cap is accepted; cap + 1 is rejected before any state change.
        #[test]
        fn debug_life_change_amount_cap_is_enforced_before_mutation() {
            let mut scenario = scenario();
            add_card(&mut scenario, P0, "Rhox Faithmender");
            let mut runner = debug_runner(scenario);

            let over = DebugAction::GainLife {
                player_id: P0,
                amount: MAX_DEBUG_LIFE_AMOUNT + 1,
            };
            assert!(preflight_debug_action(runner.state(), P0, &over).is_err());
            let revision = runner.state().state_revision;
            assert!(runner.act(GameAction::Debug(over)).is_err());
            assert_eq!(life(&runner, P0), 20);
            assert_eq!(runner.state().state_revision, revision);
            let over_loss = DebugAction::LoseLife {
                player_id: P0,
                amount: MAX_DEBUG_LIFE_AMOUNT + 1,
            };
            assert!(preflight_debug_action(runner.state(), P0, &over_loss).is_err());

            act_ok(&mut runner, gain(P0, MAX_DEBUG_LIFE_AMOUNT));
            let expected = 20 + 2 * i32::try_from(MAX_DEBUG_LIFE_AMOUNT).unwrap();
            assert_eq!(life(&runner, P0), expected);
        }

        /// Unrepresentable final results are rejected unchanged by the action
        /// boundary — no wrap, no clamp.
        #[test]
        fn debug_life_change_rejects_unrepresentable_results_unchanged() {
            type Setup = fn(&mut GameState);
            let cases: [(Setup, GameAction); 4] = [
                (|s| s.players[0].life = i32::MAX - 1, gain(P0, 2)),
                (|s| s.players[0].life = i32::MIN + 1, lose(P0, 2)),
                (
                    |s| s.players[0].life_gained_this_turn = u32::MAX - 1,
                    gain(P0, 2),
                ),
                (
                    |s| s.players[0].life_lost_this_turn = u32::MAX - 1,
                    lose(P0, 2),
                ),
            ];
            for (index, (setup, action)) in cases.into_iter().enumerate() {
                let mut scenario = scenario();
                let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
                let mut runner = debug_runner(scenario);
                setup(runner.state_mut());
                let before = runner.state().players[0].clone();
                let revision = runner.state().state_revision;

                let error = runner
                    .act(action)
                    .expect_err(&format!("case {index} must be rejected"));
                assert_not_representable(&error);
                let after = &runner.state().players[0];
                assert_eq!(after.life, before.life, "case {index}");
                assert_eq!(
                    after.life_gained_this_turn, before.life_gained_this_turn,
                    "case {index}"
                );
                assert_eq!(
                    after.life_lost_this_turn, before.life_lost_this_turn,
                    "case {index}"
                );
                assert_eq!(runner.state().state_revision, revision, "case {index}");
                assert!(runner.state().stack.is_empty(), "case {index}");
                assert_eq!(p1p1(&runner, mate), 0, "case {index}");
            }
        }

        /// The final gain edit is the representability authority: Rhox Faithmender
        /// ("If you would gain life, you gain twice that much life instead.")
        /// doubles a requested 1 to 2, and a life total of `i32::MAX - 1` cannot
        /// take it. The action is rejected unchanged; one below the limit it
        /// applies in full.
        #[test]
        fn debug_gain_final_life_overflow_after_a_parsed_doubler_is_rejected_unchanged() {
            // Accepted control: 1 doubled to 2 reaches i32::MAX exactly.
            let mut scenario = scenario();
            scenario.with_life(P0, i32::MAX - 2);
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Rhox Faithmender");
            let mut runner = debug_runner(scenario);
            act_ok(&mut runner, gain(P0, 1));
            assert_eq!(life(&runner, P0), i32::MAX);
            runner.advance_until_stack_empty();
            assert_eq!(p1p1(&runner, mate), 1);

            let mut scenario = self::scenario();
            scenario.with_life(P0, i32::MAX - 1);
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Rhox Faithmender");
            let mut runner = debug_runner(scenario);
            let before = rollback_set(&runner, mate);
            let error = runner
                .act(gain(P0, 1))
                .expect_err("a doubled gain past i32::MAX must be rejected, not panic");
            assert_not_representable(&error);
            assert_eq!(rollback_set(&runner, mate), before);
        }

        /// The gained-this-turn counter is checked by the same final edit.
        #[test]
        fn debug_gain_final_counter_overflow_after_a_parsed_doubler_is_rejected_unchanged() {
            let mut scenario = scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Rhox Faithmender");
            let mut runner = debug_runner(scenario);
            runner.state_mut().players[0].life_gained_this_turn = u32::MAX - 2;
            act_ok(&mut runner, gain(P0, 1));
            assert_eq!(runner.state().players[0].life_gained_this_turn, u32::MAX);
            runner.advance_until_stack_empty();
            assert_eq!(p1p1(&runner, mate), 1);

            let mut scenario = self::scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Rhox Faithmender");
            let mut runner = debug_runner(scenario);
            runner.state_mut().players[0].life_gained_this_turn = u32::MAX - 1;
            let before = rollback_set(&runner, mate);
            let error = runner
                .act(gain(P0, 1))
                .expect_err("a doubled gain past the per-turn counter must be rejected");
            assert_not_representable(&error);
            assert_eq!(rollback_set(&runner, mate), before);
        }

        /// A choice-resumed delivery is rejected the same way: Rhox Faithmender
        /// and Boon Reflection make the player order the doublers (CR 616.1),
        /// 1 doubled twice is 4, and `i32::MAX - 3` cannot take it. The
        /// delivering answer is rejected, the accepted proposal and prompt are
        /// restored and stay answerable.
        #[test]
        fn debug_gain_choice_resumed_overflow_is_rejected_and_the_prompt_stays_answerable() {
            // Accepted control: i32::MAX - 4 + 4 reaches i32::MAX.
            let mut scenario = scenario();
            scenario.with_life(P0, i32::MAX - 4);
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Rhox Faithmender");
            add_card(&mut scenario, P0, "Boon Reflection");
            let mut runner = debug_runner(scenario);
            act_ok(&mut runner, gain(P0, 1));
            assert!(
                replacement_prompt_open(&runner),
                "reach guard: two doublers must park an ordering prompt, got {:?}",
                runner.state().waiting_for
            );
            answer_to_completion(&mut runner);
            assert_eq!(life(&runner, P0), i32::MAX);
            runner.advance_until_stack_empty();
            assert_eq!(p1p1(&runner, mate), 1);

            let mut scenario = self::scenario();
            scenario.with_life(P0, i32::MAX - 3);
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Rhox Faithmender");
            add_card(&mut scenario, P0, "Boon Reflection");
            let mut runner = debug_runner(scenario);
            act_ok(&mut runner, gain(P0, 1));
            assert!(
                replacement_prompt_open(&runner),
                "reach guard: two doublers must park an ordering prompt, got {:?}",
                runner.state().waiting_for
            );
            assert_eq!(
                parked_policy(&runner),
                Some(UnrepresentablePolicy::Reject),
                "reach guard: a debug gain's parked proposal is stamped for rejection"
            );
            assert_choice_rejected_and_still_answerable(&mut runner, mate);
            assert_eq!(life(&runner, P0), i32::MAX - 3);
        }

        /// CR 616.1f: a re-parked record keeps the rejection stamp. Three
        /// doublers re-park the ordering prompt after the first answer; the
        /// delivering (third) application of 1 -> 2 -> 4 -> 8 overflows
        /// `i32::MAX - 7` and is rejected.
        #[test]
        fn debug_gain_reparked_choice_keeps_the_rejection_stamp() {
            // Accepted control: i32::MAX - 8 + 8 reaches i32::MAX.
            let mut scenario = scenario();
            scenario.with_life(P0, i32::MAX - 8);
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Rhox Faithmender");
            add_card(&mut scenario, P0, "Boon Reflection");
            add_card(&mut scenario, P0, "Alhammarret's Archive");
            let mut runner = debug_runner(scenario);
            act_ok(&mut runner, gain(P0, 1));
            answer_to_completion(&mut runner);
            assert_eq!(life(&runner, P0), i32::MAX);
            runner.advance_until_stack_empty();
            assert_eq!(p1p1(&runner, mate), 1);

            let mut scenario = self::scenario();
            scenario.with_life(P0, i32::MAX - 7);
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Rhox Faithmender");
            add_card(&mut scenario, P0, "Boon Reflection");
            add_card(&mut scenario, P0, "Alhammarret's Archive");
            let mut runner = debug_runner(scenario);
            act_ok(&mut runner, gain(P0, 1));
            assert_eq!(parked_policy(&runner), Some(UnrepresentablePolicy::Reject));
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect("the first answer only applies one doubler and re-parks");
            assert!(
                replacement_prompt_open(&runner),
                "reach guard: the first answer must re-park an ordering prompt, got {:?}",
                runner.state().waiting_for
            );
            let reparked = runner
                .state()
                .pending_replacement
                .as_ref()
                .expect("a re-parked record is pending");
            assert_eq!(
                reparked.candidates.len(),
                2,
                "reach guard: two doublers remain to be ordered"
            );
            assert_eq!(
                reparked.unrepresentable_life_change,
                UnrepresentablePolicy::Reject,
                "the re-parked record must keep the rejection stamp"
            );
            assert_choice_rejected_and_still_answerable(&mut runner, mate);
            assert_eq!(life(&runner, P0), i32::MAX - 7);
        }

        /// Puts one P0 permanent per entry of `factors` in play, each carrying a
        /// `Multiply { factor, EventContextAmount }` replacement execute of
        /// `event` (the shape Boon Reflection / Bloodletter of Aclazotz parse to,
        /// with a chosen factor). Distinct objects with an execute body are
        /// order-material, so every application but the last is a CR 616.1
        /// ordering prompt. The factors are chosen so that the overflow needs
        /// three candidates, not a dozen doubling cards.
        fn add_life_multipliers(runner: &mut GameRunner, event: ReplacementEvent, factors: &[i32]) {
            use crate::types::ability::{QuantityRef, ReplacementPlayerScope};
            for (index, factor) in factors.iter().enumerate() {
                let amount = QuantityExpr::Multiply {
                    factor: *factor,
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    }),
                };
                let effect = match event {
                    ReplacementEvent::GainLife => Effect::GainLife {
                        amount,
                        player: TargetFilter::Controller,
                    },
                    ReplacementEvent::LoseLife => Effect::LoseLife {
                        amount,
                        target: None,
                    },
                    _ => panic!("add_life_multipliers supports GainLife and LoseLife only"),
                };
                let mut definition = ReplacementDefinition::new(event.clone())
                    .execute(AbilityDefinition::new(AbilityKind::Spell, effect));
                definition.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
                let id = create_object(
                    runner.state_mut(),
                    CardId(9_900 + index as u64),
                    P0,
                    format!("Life Multiplier {index}"),
                    Zone::Battlefield,
                );
                let object = runner.state_mut().objects.get_mut(&id).unwrap();
                object.replacement_definitions = vec![definition.clone()].into();
                object.base_replacement_definitions = std::sync::Arc::new(vec![definition]);
            }
        }

        /// Engine numeric capacity + CR 616.1: a parsed Boon Reflection ("If you would gain life,
        /// you gain twice that much life instead.") and two hand-built x1,000
        /// multipliers would take a requested 1,000,000 to 2,000,000,000,000, past
        /// an `i32`. Prompts are asked while two or more candidates remain (three,
        /// then two): the first answer applies one candidate (at most
        /// 1,000,000 x 1,000 = 1,000,000,000, which fits), and the second applies
        /// another (at most 2,000,000,000, which fits) and then auto-applies the
        /// last, whose checked multiply reports the result as unrepresentable in
        /// any order. That answer is rejected unchanged, the prompt stays
        /// answerable, and no later prompt is asked. The accepted control (Boon
        /// Reflection with x10 and x100 = 2 x 10 x 100 x 1,000,000 =
        /// 2,000,000,000) fits and applies in full.
        #[test]
        fn debug_gain_large_multipliers_reject_the_overflowing_application() {
            // Accepted control: the product 2,000 x 1,000,000 fits an i32.
            let mut scenario = scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Boon Reflection");
            let mut runner = debug_runner(scenario);
            add_life_multipliers(&mut runner, ReplacementEvent::GainLife, &[10, 100]);
            act_ok(&mut runner, gain(P0, MAX_DEBUG_LIFE_AMOUNT));
            assert!(
                replacement_prompt_open(&runner),
                "reach guard: three doublers park an ordering prompt, got {:?}",
                runner.state().waiting_for
            );
            answer_to_completion(&mut runner);
            assert_eq!(life(&runner, P0), 20 + 2_000_000_000);
            assert_eq!(
                runner.state().players[0].life_gained_this_turn,
                2_000_000_000
            );
            runner.advance_until_stack_empty();
            assert_eq!(p1p1(&runner, mate), 1);

            // Overflow: the product 2,000,000 x 1,000,000 does not fit.
            let mut scenario = self::scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Boon Reflection");
            let mut runner = debug_runner(scenario);
            add_life_multipliers(&mut runner, ReplacementEvent::GainLife, &[1_000, 1_000]);
            act_ok(&mut runner, gain(P0, MAX_DEBUG_LIFE_AMOUNT));
            assert!(
                replacement_prompt_open(&runner),
                "reach guard: three doublers park an ordering prompt, got {:?}",
                runner.state().waiting_for
            );
            assert_eq!(
                parked_policy(&runner),
                Some(UnrepresentablePolicy::Reject),
                "reach guard: a debug gain's parked proposal is stamped for rejection"
            );
            let accepted = assert_choice_rejected_and_still_answerable(&mut runner, mate);
            // Prompts are asked while two or more candidates remain: the first
            // answer applies one candidate (fits) and leaves two, the second
            // answer applies another (fits) and then auto-applies the last, which
            // overflows. The exact count is not load-bearing here (the
            // loop-head stop is pinned by the next row); some answer was
            // accepted before the rejection.
            assert!(
                (1..=2).contains(&accepted),
                "accepted {accepted} answers before the rejection"
            );
            assert_eq!(life(&runner, P0), 20);
            assert_eq!(
                runner.state().replacement_quantity_overflow,
                None,
                "the rejected action's snapshot restore clears the record"
            );
        }

        /// Engine numeric capacity + CR 616.1f: the loop-head stop, through the production
        /// action. Four x100 multipliers on 1,000,000: the first application
        /// gives 100,000,000 (fits) and the second 10,000,000,000, which
        /// overflows an `i32` in any order. Prompts are asked only while two or
        /// more applicable replacements remain: the first prompt (four
        /// candidates) is answered and accepted, leaving three; the second
        /// answer applies the overflowing multiplier, and the repetition stops
        /// there, so the answer is rejected. Exactly one answer is accepted. If
        /// the loop kept going after the overflow, two candidates would remain,
        /// the second answer would re-park a prompt and be accepted, and the
        /// rejection would only come with the third answer (`accepted == 2`).
        #[test]
        fn debug_gain_overflow_stops_the_repetition_before_the_remaining_multipliers() {
            let mut scenario = scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            let mut runner = debug_runner(scenario);
            add_life_multipliers(
                &mut runner,
                ReplacementEvent::GainLife,
                &[100, 100, 100, 100],
            );
            act_ok(&mut runner, gain(P0, MAX_DEBUG_LIFE_AMOUNT));
            assert_eq!(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .map(|pending| pending.candidates.len()),
                Some(4),
                "reach guard: all four multipliers are ordered at the first prompt"
            );
            let accepted = assert_choice_rejected_and_still_answerable(&mut runner, mate);
            assert_eq!(
                accepted, 1,
                "the overflow on the second application ends the repetition"
            );
            assert_eq!(life(&runner, P0), 20);
            assert_eq!(runner.state().replacement_quantity_overflow, None);
        }

        /// Bloodletter of Aclazotz ("If an opponent would lose life during your
        /// turn, they lose twice that much life instead.") parsed from its
        /// Oracle text: eleven copies turn a requested 1,000,000 into
        /// 1,000,000 x 2^11 = 2,048,000,000, which fits (accepted control); the
        /// twelfth makes 4,096,000,000, which fits a `u32` but not an `i32` delta
        /// and is rejected unchanged.
        #[test]
        fn debug_lose_parsed_doublers_accept_the_representable_and_reject_the_i32_overflow() {
            let mut scenario = scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            let mut runner = debug_runner(scenario);
            add_bloodletters(&mut runner, 11);
            act_ok(&mut runner, lose(P1, MAX_DEBUG_LIFE_AMOUNT));
            answer_to_completion(&mut runner);
            assert_eq!(
                life(&runner, P1),
                20 - 2_048_000_000,
                "reach guard: eleven doublers apply (CR 616.1f repeats until none remain)"
            );
            assert_eq!(runner.state().players[1].life_lost_this_turn, 2_048_000_000);
            assert_eq!(p1p1(&runner, mate), 0);

            let mut scenario = self::scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            let mut runner = debug_runner(scenario);
            add_bloodletters(&mut runner, 12);
            submit_expecting_rejection(&mut runner, mate, lose(P1, MAX_DEBUG_LIFE_AMOUNT));
            assert_eq!(life(&runner, P1), 20);
        }

        /// Engine numeric capacity: the loss twin of the large-multiplier gain row. Three
        /// multipliers (x10, x10, x1,000) take 1,000,000 past an `i32`; the loss is
        /// rejected unchanged. The loop-stop discrimination is in the `replacement`
        /// unit row `lose_life_times_overflow_stops_the_loop_at_the_overflowing_replacement`.
        #[test]
        fn debug_lose_large_multipliers_reject_the_overflowing_application() {
            let mut scenario = scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            let mut runner = debug_runner(scenario);
            add_life_multipliers(&mut runner, ReplacementEvent::LoseLife, &[10, 10, 1_000]);
            submit_expecting_rejection(&mut runner, mate, lose(P1, MAX_DEBUG_LIFE_AMOUNT));
            assert_eq!(life(&runner, P1), 20);
            assert_eq!(runner.state().replacement_quantity_overflow, None);
        }

        /// One parsed doubler: the final loss overflows the life total
        /// (`i32::MIN + 3` losing 2 doubled to 4) or the lost-this-turn counter
        /// (`u32::MAX - 3` losing 2 doubled to 4), and is rejected unchanged; one
        /// below each limit applies.
        #[test]
        fn debug_lose_final_overflow_after_a_parsed_doubler_is_rejected_unchanged() {
            // (case, P1 life, P1 lost-this-turn counter, accepted result)
            type LossCase = (&'static str, i32, u32, Option<(i32, u32)>);
            let cases: [LossCase; 4] = [
                ("life total, accepted", i32::MIN + 4, 0, Some((i32::MIN, 4))),
                ("life total, rejected", i32::MIN + 3, 0, None),
                ("counter, accepted", 20, u32::MAX - 4, Some((16, u32::MAX))),
                ("counter, rejected", 20, u32::MAX - 3, None),
            ];
            for (name, start_life, lost_counter, accepted) in cases {
                let mut scenario = scenario();
                scenario.with_life(P1, start_life);
                let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
                let mut runner = debug_runner(scenario);
                runner.state_mut().players[1].life_lost_this_turn = lost_counter;
                add_bloodletters(&mut runner, 1);
                match accepted {
                    Some((life_after, counter_after)) => {
                        act_ok(&mut runner, lose(P1, 2));
                        answer_to_completion(&mut runner);
                        let player = &runner.state().players[1];
                        assert_eq!(
                            (player.life, player.life_lost_this_turn),
                            (life_after, counter_after),
                            "{name}: the doubled loss (2 -> 4) must apply in full"
                        );
                    }
                    None => submit_expecting_rejection(&mut runner, mate, lose(P1, 2)),
                }
            }
        }

        /// A cross-event substitute of the ingress gain is held to the same rule:
        /// Tainted Remedy ("If an opponent would gain life, that player loses
        /// that much life instead.") turns P0's gain of 2 into a loss of 2, and
        /// that substituted loss is not representable, so the action is rejected
        /// instead of the loss being silently skipped.
        #[test]
        fn debug_gain_replaced_by_an_unrepresentable_substitute_loss_is_rejected() {
            // (life total, per-turn lost counter, gain, accepted)
            let cases = [
                (i32::MIN + 2, 0, 2, true),
                (i32::MIN + 1, 0, 2, false),
                (20, u32::MAX - 2, 2, true),
                (20, u32::MAX - 1, 2, false),
            ];
            for (start_life, lost_counter, amount, accepted) in cases {
                let mut scenario = scenario();
                scenario.with_life(P0, start_life);
                let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
                add_card(&mut scenario, P1, "Tainted Remedy");
                let mut runner = debug_runner(scenario);
                runner.state_mut().players[0].life_lost_this_turn = lost_counter;
                if accepted {
                    act_ok(&mut runner, gain(P0, amount));
                    let player = &runner.state().players[0];
                    assert_eq!(
                        player.life_gained_this_turn, 0,
                        "reach guard: the gain was replaced by a loss (start {start_life})"
                    );
                    assert_eq!(
                        player.life,
                        start_life - amount as i32,
                        "the substituted loss applies when representable"
                    );
                    assert_eq!(p1p1(&runner, mate), 0, "no gain occurred, so no trigger");
                } else {
                    let before = rollback_set(&runner, mate);
                    let error = runner
                        .act(gain(P0, amount))
                        .expect_err("an unrepresentable substituted loss must reject the action");
                    assert_not_representable(&error);
                    assert_eq!(rollback_set(&runner, mate), before);
                }
            }
        }

        /// The substitute can also be chosen at the ordering prompt. Tainted
        /// Remedy and Boon Reflection ("If you would gain life, you gain twice
        /// that much life instead.") both apply to P0's gain, so the player
        /// orders them (CR 616.1). The overflow is in the lost-this-turn counter
        /// (`u32::MAX`), never the life total: a life total at or below 0 would
        /// make the action boundary's state-based actions end the game before
        /// the prompt could be observed (CR 704.5a), so P0 stays at 20 life.
        /// At `u32::MAX` both orders end in an unrepresentable loss (lose 1, or
        /// gain 2 then lose 2); whichever answer delivers it is rejected and the
        /// prompt stays answerable. At `u32::MAX - 2` both orders apply.
        #[test]
        fn debug_gain_resumed_substitute_loss_overflow_is_rejected_and_answerable() {
            let mut scenario = scenario();
            add_card(&mut scenario, P0, "Boon Reflection");
            add_card(&mut scenario, P1, "Tainted Remedy");
            let mut runner = debug_runner(scenario);
            runner.state_mut().players[0].life_lost_this_turn = u32::MAX - 2;
            act_ok(&mut runner, gain(P0, 1));
            assert!(
                replacement_prompt_open(&runner),
                "reach guard: both replacements apply, so an ordering prompt opens, got {:?}",
                runner.state().waiting_for
            );
            answer_to_completion(&mut runner);
            let player = &runner.state().players[0];
            assert!(
                (player.life, player.life_lost_this_turn) == (19, u32::MAX - 1)
                    || (player.life, player.life_lost_this_turn) == (18, u32::MAX),
                "Tainted first loses 1 (19, MAX - 1); Boon first gains 2 then loses 2 (18, MAX); \
                 got ({}, {})",
                player.life,
                player.life_lost_this_turn
            );

            let mut scenario = self::scenario();
            let mate = add_card(&mut scenario, P0, "Ajani's Pridemate");
            add_card(&mut scenario, P0, "Boon Reflection");
            add_card(&mut scenario, P1, "Tainted Remedy");
            let mut runner = debug_runner(scenario);
            runner.state_mut().players[0].life_lost_this_turn = u32::MAX;
            act_ok(&mut runner, gain(P0, 1));
            assert!(
                replacement_prompt_open(&runner),
                "reach guard: both replacements apply, so an ordering prompt opens, got {:?}",
                runner.state().waiting_for
            );
            assert_eq!(
                parked_policy(&runner),
                Some(UnrepresentablePolicy::Reject),
                "reach guard: a debug gain's parked proposal is stamped for rejection"
            );
            assert_choice_rejected_and_still_answerable(&mut runner, mate);
            let player = &runner.state().players[0];
            assert_eq!((player.life, player.life_lost_this_turn), (20, u32::MAX));
        }

        /// The sandbox audit log describes both actions (engine-owned wording).
        #[test]
        fn debug_life_change_describe_strings() {
            let state = sandbox_state();
            assert_eq!(
                DebugAction::GainLife {
                    player_id: PlayerId(0),
                    amount: 3
                }
                .describe(&state),
                "GainLife (Player 1 gains 3)"
            );
            assert_eq!(
                DebugAction::LoseLife {
                    player_id: PlayerId(1),
                    amount: 2
                }
                .describe(&state),
                "LoseLife (Player 2 loses 2)"
            );
        }

        #[test]
        fn debug_life_change_requires_debug_mode() {
            let mut runner = scenario().build();
            assert!(runner.act(gain(P0, 1)).is_err());
            assert_eq!(life(&runner, P0), 20);
        }

        #[test]
        fn debug_life_change_rejects_unknown_player() {
            let mut runner = debug_runner(scenario());
            assert!(runner.act(gain(PlayerId(9), 1)).is_err());
            assert!(runner.act(lose(PlayerId(9), 1)).is_err());
        }

        /// A player who left the game cannot receive a debug life change; the
        /// remaining players still can.
        #[test]
        fn debug_life_change_rejects_eliminated_recipient() {
            let mut scenario = GameScenario::new_n_player(3, 42);
            scenario.at_phase(Phase::PreCombatMain);
            let mut runner = debug_runner(scenario);
            let p2 = PlayerId(2);
            let mut events = Vec::new();
            crate::game::elimination::eliminate_player(runner.state_mut(), p2, &mut events);
            let p2_life = life(&runner, p2);
            let revision = runner.state().state_revision;

            assert!(runner.act(gain(p2, 1)).is_err());
            assert_eq!(life(&runner, p2), p2_life);
            assert_eq!(runner.state().state_revision, revision);

            act_ok(&mut runner, gain(P1, 1));
            assert_eq!(life(&runner, P1), 21);
        }
    }

    // ── Player-counter and energy gains settle through the checked authority ──
    //
    // Presets are written straight to the player fields, never through the raw
    // `ModifyPlayerCounters` / `ModifyEnergy` editors.

    mod player_counter_settlement {
        use super::life_change::{
            add_card, debug_runner, p1p1, parked_policy, replacement_prompt_open, scenario,
        };
        use super::*;
        use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
        use crate::game::scenario_db::GameScenarioDbExt;
        use crate::types::ability::{QuantityModification, ReplacementPlayerScope};
        use crate::types::game_state::{PendingReplacement, UnrepresentablePolicy};
        use std::collections::HashMap;

        /// Everything the action boundary must restore exactly when it rejects an
        /// action: every player's poison, generic counters and energy, the state
        /// revision, the prompt and its parked proposal, the stack and the
        /// resolved rules journal.
        #[derive(Debug, PartialEq)]
        struct CounterRollbackSet {
            players: Vec<(u32, HashMap<PlayerCounterKind, u32>, u32)>,
            state_revision: u64,
            waiting_for: WaitingFor,
            pending_replacement: Option<PendingReplacement>,
            stack_len: usize,
            journal_len: usize,
        }

        fn counter_rollback_set(runner: &GameRunner) -> CounterRollbackSet {
            let state = runner.state();
            CounterRollbackSet {
                players: state
                    .players
                    .iter()
                    .map(|player| {
                        (
                            player.poison_counters,
                            player.player_counters.clone(),
                            player.energy,
                        )
                    })
                    .collect(),
                state_revision: state.state_revision,
                waiting_for: state.waiting_for.clone(),
                pending_replacement: state.pending_replacement.clone(),
                stack_len: state.stack.len(),
                journal_len: state.resolved_rules_journal.entries().len(),
            }
        }

        fn assert_unrepresentable_for(error: &EngineError, player: PlayerId) {
            assert!(
                matches!(
                    error,
                    EngineError::LifeChangeUnrepresentable { player: failed } if *failed == player
                ),
                "expected the typed unrepresentable rejection for {player:?}, got {error:?}"
            );
        }

        /// The two player resources a gain can settle: a generic player counter
        /// (experience) and energy.
        #[derive(Clone, Copy)]
        enum Resource {
            Experience,
            Energy,
        }

        impl Resource {
            fn get(self, runner: &GameRunner, player: PlayerId) -> u32 {
                let player = &runner.state().players[player.0 as usize];
                match self {
                    Resource::Experience => player
                        .player_counters
                        .get(&PlayerCounterKind::Experience)
                        .copied()
                        .unwrap_or(0),
                    Resource::Energy => player.energy,
                }
            }

            fn set(self, runner: &mut GameRunner, player: PlayerId, value: u32) {
                let player = &mut runner.state_mut().players[player.0 as usize];
                match self {
                    Resource::Experience => {
                        player
                            .player_counters
                            .insert(PlayerCounterKind::Experience, value);
                    }
                    Resource::Energy => player.energy = value,
                }
            }

            /// The deltas of this resource's change events for `player`.
            fn deltas(self, events: &[GameEvent], player: PlayerId) -> Vec<i32> {
                events
                    .iter()
                    .filter_map(|event| match (self, event) {
                        (
                            Resource::Experience,
                            GameEvent::PlayerCounterChanged {
                                player: changed,
                                counter_kind: PlayerCounterKind::Experience,
                                delta,
                            },
                        )
                        | (
                            Resource::Energy,
                            GameEvent::EnergyChanged {
                                player: changed,
                                delta,
                            },
                        ) if *changed == player => Some(*delta),
                        _ => None,
                    })
                    .collect()
            }
        }

        /// Puts a P-controlled permanent in play carrying an any-player
        /// `AddCounter` count replacement, set on both the live and base
        /// definitions so a layer pass keeps it.
        fn add_counter_quantity_source(
            runner: &mut GameRunner,
            controller: PlayerId,
            modification: QuantityModification,
        ) -> ObjectId {
            let mut definition = ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .quantity_modification(modification);
            definition.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
            let serial = runner.state().objects.len() as u64;
            let id = create_object(
                runner.state_mut(),
                CardId(9_800 + serial),
                controller,
                format!("Counter Scaler {serial}"),
                Zone::Battlefield,
            );
            let object = runner.state_mut().objects.get_mut(&id).unwrap();
            object.replacement_definitions = vec![definition.clone()].into();
            object.base_replacement_definitions = std::sync::Arc::new(vec![definition]);
            id
        }

        /// Submits the debug `Proliferate` for P0 and asserts the reach guards:
        /// the action was audited as used, and the real choice is open with the
        /// recipient eligible.
        fn proliferate_to_choice(runner: &mut GameRunner, recipient: PlayerId) {
            let result = runner
                .act(GameAction::Debug(DebugAction::Proliferate {
                    player_id: P0,
                }))
                .expect("debug proliferate must start");
            assert!(
                result.events.iter().any(|event| matches!(
                    event,
                    GameEvent::DebugActionUsed { player_id, .. } if *player_id == P0
                )),
                "reach guard: the debug action must be audited as used by P0"
            );
            let WaitingFor::ProliferateChoice { player, eligible } = &runner.state().waiting_for
            else {
                panic!(
                    "reach guard: expected ProliferateChoice, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(*player, P0);
            assert!(
                eligible.contains(&TargetRef::Player(recipient)),
                "reach guard: the recipient must be eligible, got {eligible:?}"
            );
        }

        fn select(recipient: PlayerId) -> GameAction {
            GameAction::SelectTargets {
                targets: vec![TargetRef::Player(recipient)],
            }
        }

        fn assert_priority_with_empty_stack(runner: &GameRunner) {
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                "expected Priority, got {:?}",
                runner.state().waiting_for
            );
            assert!(runner.state().stack.is_empty());
        }

        /// CR 701.34a: proliferate gives P0 one more of the resource. A final
        /// total past `u32` rejects the action with the typed error, restores the
        /// entry state, and leaves the choice answerable (decline completes it).
        fn ordinary_final_edit_overflow(resource: Resource) {
            let mut runner = debug_runner(scenario());
            resource.set(&mut runner, P0, u32::MAX);
            proliferate_to_choice(&mut runner, P0);
            let before = counter_rollback_set(&runner);

            let error = runner
                .act(select(P0))
                .expect_err("an overflowing final edit must reject the action");

            assert_unrepresentable_for(&error, P0);
            assert_eq!(counter_rollback_set(&runner), before);
            assert_eq!(resource.get(&runner, P0), u32::MAX);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ProliferateChoice { .. }
            ));
            runner
                .act(GameAction::SelectTargets { targets: vec![] })
                .expect("declining the restored choice completes it");
            assert_priority_with_empty_stack(&runner);
            assert_eq!(resource.get(&runner, P0), u32::MAX);

            // Control: one below the limit takes the gain.
            let mut runner = debug_runner(scenario());
            resource.set(&mut runner, P0, u32::MAX - 1);
            proliferate_to_choice(&mut runner, P0);
            let result = runner
                .act(select(P0))
                .expect("a representable gain must apply");
            assert_eq!(resource.get(&runner, P0), u32::MAX);
            assert_eq!(resource.deltas(&result.events, P0), vec![1]);
            assert_priority_with_empty_stack(&runner);
        }

        /// O1: ordinary resolution, player counter, checked final edit.
        #[test]
        fn proliferate_player_counter_total_overflow_rejects_and_rolls_back() {
            ordinary_final_edit_overflow(Resource::Experience);
        }

        /// O2: ordinary resolution, energy, checked final edit.
        #[test]
        fn proliferate_energy_total_overflow_rejects_and_rolls_back() {
            ordinary_final_edit_overflow(Resource::Energy);
        }

        /// O3, CR 603.2 + CR 603.3: Aether Hub's "you get {E}" trigger goes on the
        /// stack and resolves on the priority passes. A replacement that takes the
        /// gain past `i32::MAX` (but within `u32`) cannot be a delta; the resolving
        /// pass is rejected and rolled back instead of wrapping to a negative
        /// delta.
        #[test]
        fn aether_hub_energy_gain_past_i32_rejects_the_resolving_pass() {
            let hub_land = |factor: u32| {
                let mut scenario = scenario();
                let hub = scenario.add_real_card(
                    P0,
                    "Aether Hub",
                    Zone::Hand,
                    crate::test_support::shared_card_db(),
                );
                let mut runner = debug_runner(scenario);
                add_counter_quantity_source(
                    &mut runner,
                    P0,
                    QuantityModification::Times { factor },
                );
                let card_id = runner.state().objects[&hub].card_id;
                runner
                    .act(GameAction::PlayLand {
                        object_id: hub,
                        card_id,
                    })
                    .expect("Aether Hub must be playable");
                let stack = &runner.state().stack;
                assert!(
                    stack.len() == 1 && stack.iter().all(|entry| entry.source_id == hub),
                    "reach guard: the ETB trigger must be on the stack"
                );
                (runner, hub)
            };

            let (mut runner, hub) = hub_land(u32::MAX);
            let mut rejection = None;
            for _ in 0..4 {
                assert!(
                    runner
                        .state()
                        .stack
                        .iter()
                        .any(|entry| entry.source_id == hub),
                    "the trigger must still be on the stack before the resolving pass"
                );
                let before = counter_rollback_set(&runner);
                match runner.act(GameAction::PassPriority) {
                    Ok(_) => {}
                    Err(error) => {
                        rejection = Some((before, error));
                        break;
                    }
                }
            }
            let (before, error) = rejection.expect("the resolving pass must be rejected");
            assert_unrepresentable_for(&error, P0);
            assert_eq!(counter_rollback_set(&runner), before);
            assert_eq!(Resource::Energy.get(&runner, P0), 0);

            // Control: exactly `i32::MAX` is a representable delta.
            let (mut runner, _) = hub_land(i32::MAX as u32);
            let mut events = Vec::new();
            for _ in 0..4 {
                if runner.state().stack.is_empty() {
                    break;
                }
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("a representable gain must resolve")
                        .events,
                );
            }
            assert_eq!(Resource::Energy.get(&runner, P0), i32::MAX as u32);
            assert_eq!(Resource::Energy.deltas(&events, P0), vec![i32::MAX]);
            assert_priority_with_empty_stack(&runner);
        }

        /// O4: a replacement's own arithmetic overflowing `u32` is recorded by the
        /// applier, consumed by the settle step, and rejected at the boundary.
        #[test]
        fn proliferate_replacement_count_overflow_rejects_and_rolls_back() {
            let mut runner = debug_runner(scenario());
            Resource::Experience.set(&mut runner, P0, 1);
            add_counter_quantity_source(
                &mut runner,
                P0,
                QuantityModification::Plus { value: u32::MAX },
            );
            proliferate_to_choice(&mut runner, P0);
            let before = counter_rollback_set(&runner);

            let error = runner
                .act(select(P0))
                .expect_err("an overflowing replacement count must reject the action");

            assert_unrepresentable_for(&error, P0);
            assert_eq!(counter_rollback_set(&runner), before);
            assert_eq!(Resource::Experience.get(&runner, P0), 1);

            // Control: a representable `Plus` applies to the proliferate
            // placement (1 + (1 + 1)), proving the source reaches this seam.
            let mut runner = debug_runner(scenario());
            Resource::Experience.set(&mut runner, P0, 1);
            add_counter_quantity_source(&mut runner, P0, QuantityModification::Plus { value: 1 });
            proliferate_to_choice(&mut runner, P0);
            let result = runner
                .act(select(P0))
                .expect("a representable gain applies");
            assert_eq!(Resource::Experience.get(&runner, P0), 3);
            assert_eq!(Resource::Experience.deltas(&result.events, P0), vec![2]);
            assert_priority_with_empty_stack(&runner);
        }

        /// A battlefield with Vorinclex, Monstrous Raider under each player: P0's
        /// doubles the counters P0 puts, P1's halves the counters an opponent
        /// (P0) puts.
        fn vorinclex_pair() -> (GameScenario, ObjectId, ObjectId) {
            let mut scenario = scenario();
            let p0_vorinclex = add_card(&mut scenario, P0, "Vorinclex, Monstrous Raider");
            let p1_vorinclex = add_card(&mut scenario, P1, "Vorinclex, Monstrous Raider");
            (scenario, p0_vorinclex, p1_vorinclex)
        }

        /// Parks the CR 616.1 prompt for P0 proliferating P1: returns the
        /// candidate indices of P0's doubler and P1's halver.
        fn park_vorinclex_prompt(
            runner: &mut GameRunner,
            p0_vorinclex: ObjectId,
            p1_vorinclex: ObjectId,
        ) -> (usize, usize) {
            proliferate_to_choice(runner, P1);
            runner
                .act(select(P1))
                .expect("selecting the recipient must park the ordering prompt");
            let WaitingFor::ReplacementChoice {
                player,
                candidate_count,
                candidates,
                ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "reach guard: expected the ordering prompt, got {:?}",
                    runner.state().waiting_for
                );
            };
            // CR 616.1: the affected player (the recipient) chooses, not the
            // proliferating actor.
            assert_eq!(*player, P1);
            assert_eq!(*candidate_count, 2);
            let index_of = |source: ObjectId| {
                candidates
                    .iter()
                    .position(|candidate| candidate.source_id == source)
                    .unwrap_or_else(|| panic!("no candidate from {source:?}: {candidates:?}"))
            };
            (index_of(p0_vorinclex), index_of(p1_vorinclex))
        }

        /// CR 616.1 + CR 616.1e: the resume settles through the fallible settle
        /// step. With the recipient at the limit, the doubler-first order
        /// (1 -> 2 -> 1) takes the final total to `u32::MAX` + 1 and its
        /// delivering answer is rejected with the typed error, leaving the prompt
        /// answerable. The halver-first order (1 -> 0; the doubler then has
        /// nothing to double) is a legal, representable answer. CR 616.1f: only
        /// still-applicable effects apply after a choice.
        fn resume_overflow(resource: Resource) {
            let (scenario, p0_vorinclex, p1_vorinclex) = vorinclex_pair();
            let mut runner = debug_runner(scenario);
            resource.set(&mut runner, P1, u32::MAX);
            let (doubler, halver) = park_vorinclex_prompt(&mut runner, p0_vorinclex, p1_vorinclex);
            // The park was made inside P0's public action, so it is stamped for
            // rejection.
            assert_eq!(parked_policy(&runner), Some(UnrepresentablePolicy::Reject));

            let before = counter_rollback_set(&runner);
            let error = runner
                .act(GameAction::ChooseReplacement { index: doubler })
                .expect_err("the overflowing delivery must be rejected");
            assert_unrepresentable_for(&error, P1);
            assert_eq!(counter_rollback_set(&runner), before);
            assert!(replacement_prompt_open(&runner));

            let again = runner
                .act(GameAction::ChooseReplacement { index: doubler })
                .expect_err("the restored prompt must reject the same answer again");
            assert_unrepresentable_for(&again, P1);
            assert_eq!(counter_rollback_set(&runner), before);

            let result = runner
                .act(GameAction::ChooseReplacement { index: halver })
                .expect("the halver-first order is representable");
            assert_eq!(resource.get(&runner, P1), u32::MAX);
            assert!(resource.deltas(&result.events, P1).is_empty());
            assert_priority_with_empty_stack(&runner);

            // Control: one below the limit, doubler first (1 -> 2 -> 1) lands
            // exactly on the limit.
            let (scenario, p0_vorinclex, p1_vorinclex) = vorinclex_pair();
            let mut runner = debug_runner(scenario);
            resource.set(&mut runner, P1, u32::MAX - 1);
            let (doubler, _) = park_vorinclex_prompt(&mut runner, p0_vorinclex, p1_vorinclex);
            let result = runner
                .act(GameAction::ChooseReplacement { index: doubler })
                .expect("a representable delivery must be accepted");
            assert_eq!(resource.get(&runner, P1), u32::MAX);
            assert_eq!(resource.deltas(&result.events, P1), vec![1]);
            assert_priority_with_empty_stack(&runner);
        }

        /// R0: CR 616.1 resume, player counter.
        #[test]
        fn replacement_choice_resume_player_counter_overflow_rejects_and_stays_answerable() {
            resume_overflow(Resource::Experience);
        }

        /// R0e: CR 616.1 resume, energy (Fabrication Module ruling 2024-06-07:
        /// effects that interact with counters a player gets also apply to
        /// energy counters).
        #[test]
        fn replacement_choice_resume_energy_overflow_rejects_and_stays_answerable() {
            resume_overflow(Resource::Energy);
        }

        /// H1, CR 614.17 + CR 614.17c: a prohibited placement can't happen, so
        /// there is nothing unrepresentable to reject. Guard row: prevention must
        /// stay ahead of any representability decision.
        #[test]
        fn solemnity_prevents_the_gain_before_representability_is_judged() {
            let mut scenario = scenario();
            let solemnity = add_card(&mut scenario, P1, "Solemnity");
            let mut runner = debug_runner(scenario);
            assert_eq!(runner.state().objects[&solemnity].zone, Zone::Battlefield);
            Resource::Experience.set(&mut runner, P0, u32::MAX);
            proliferate_to_choice(&mut runner, P0);

            let result = runner
                .act(select(P0))
                .expect("a prevented placement is not an error");

            assert_eq!(Resource::Experience.get(&runner, P0), u32::MAX);
            assert!(Resource::Experience.deltas(&result.events, P0).is_empty());
            assert_priority_with_empty_stack(&runner);
        }

        // ── Sandbox GivePlayerCounters / GiveEnergy (CR 122.1 placement event) ──
        //
        // Every row enters through the real action boundary. The submitting seat
        // is the player who puts the counters (Vorinclex ruling 2021-02-05).

        mod debug_give {
            use super::*;
            use crate::types::actions::MAX_DEBUG_COUNTER_AMOUNT;

            const VORINCLEX: &str = "Vorinclex, Monstrous Raider";

            fn give(
                player_id: PlayerId,
                counter_kind: PlayerCounterKind,
                count: u32,
            ) -> GameAction {
                GameAction::Debug(DebugAction::GivePlayerCounters {
                    player_id,
                    counter_kind,
                    count,
                })
            }

            fn give_energy(player_id: PlayerId, count: u32) -> GameAction {
                GameAction::Debug(DebugAction::GiveEnergy { player_id, count })
            }

            impl Resource {
                fn give(self, player: PlayerId, count: u32) -> GameAction {
                    match self {
                        Resource::Experience => give(player, PlayerCounterKind::Experience, count),
                        Resource::Energy => give_energy(player, count),
                    }
                }

                fn label(self) -> &'static str {
                    match self {
                        Resource::Experience => "experience",
                        Resource::Energy => "energy",
                    }
                }
            }

            /// Submits `action` as `submitter`: through the runner when that seat
            /// is the one currently expected to act, else through the public
            /// `engine::apply` transport entry with the authenticated seat.
            fn act_by(
                runner: &mut GameRunner,
                submitter: PlayerId,
                action: GameAction,
            ) -> Result<ActionResult, EngineError> {
                if runner.state().waiting_for.acting_player() == Some(submitter) {
                    runner.act(action)
                } else {
                    crate::game::engine::apply(runner.state_mut(), submitter, action)
                }
            }

            /// Applies a give and asserts the reach guard: the action was audited
            /// as used by `submitter`.
            fn give_ok(
                runner: &mut GameRunner,
                submitter: PlayerId,
                action: GameAction,
            ) -> ActionResult {
                let result = act_by(runner, submitter, action).expect("the give must apply");
                assert!(
                    result.events.iter().any(|event| matches!(
                        event,
                        GameEvent::DebugActionUsed { player_id, .. } if *player_id == submitter
                    )),
                    "reach guard: the give must be audited as used by {submitter:?}"
                );
                result
            }

            /// The `ReplacementChoice` candidate whose source is `source`.
            fn candidate_index(runner: &GameRunner, source: ObjectId) -> usize {
                let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for
                else {
                    panic!(
                        "reach guard: expected the ordering prompt, got {:?}",
                        runner.state().waiting_for
                    );
                };
                candidates
                    .iter()
                    .position(|candidate| candidate.source_id == source)
                    .unwrap_or_else(|| panic!("no candidate from {source:?}: {candidates:?}"))
            }

            /// Asserts the open CR 616.1 prompt: owned by `owner`, offering exactly
            /// `sources`, and stamped for rejection (parked inside the give).
            fn assert_order_prompt(runner: &GameRunner, owner: PlayerId, sources: &[ObjectId]) {
                let WaitingFor::ReplacementChoice {
                    player,
                    candidate_count,
                    candidates,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!(
                        "reach guard: expected the ordering prompt, got {:?}",
                        runner.state().waiting_for
                    );
                };
                assert_eq!(*player, owner, "CR 616.1: the affected player chooses");
                assert_eq!(*candidate_count, sources.len());
                assert!(
                    sources
                        .iter()
                        .all(|source| candidates.iter().any(|c| c.source_id == *source)),
                    "candidate sources must be exactly {sources:?}, got {candidates:?}"
                );
                assert_eq!(parked_policy(runner), Some(UnrepresentablePolicy::Reject));
            }

            fn poison(runner: &GameRunner, player: PlayerId) -> u32 {
                runner.state().players[player.0 as usize].poison_counters
            }

            fn vorinclex_runner() -> GameRunner {
                let mut scenario = scenario();
                add_card(&mut scenario, P0, VORINCLEX);
                debug_runner(scenario)
            }

            fn count_events(events: &[GameEvent], wanted: impl Fn(&GameEvent) -> bool) -> usize {
                events.iter().filter(|event| wanted(event)).count()
            }

            fn has_counter_event(events: &[GameEvent]) -> bool {
                events.iter().any(|event| {
                    matches!(
                        event,
                        GameEvent::PlayerCounterChanged { .. } | GameEvent::EnergyChanged { .. }
                    )
                })
            }

            /// Submits `action` expecting the typed unrepresentable rejection for
            /// `failed`, with the rollback set unchanged.
            fn assert_rejected_unrepresentable(
                runner: &mut GameRunner,
                submitter: PlayerId,
                action: GameAction,
                failed: PlayerId,
            ) {
                let before = counter_rollback_set(runner);
                let error = act_by(runner, submitter, action)
                    .expect_err("an unrepresentable give must be rejected");
                assert_unrepresentable_for(&error, failed);
                assert_eq!(
                    counter_rollback_set(runner),
                    before,
                    "the rejected give must leave the state unchanged"
                );
            }

            /// Submits `action` expecting the settled-Priority admission rejection,
            /// with the rollback set unchanged.
            fn assert_admission_rejected(
                runner: &mut GameRunner,
                submitter: PlayerId,
                action: GameAction,
                label: &str,
            ) {
                let before = counter_rollback_set(runner);
                let error = match act_by(runner, submitter, action) {
                    Ok(_) => panic!("{label}: the action must be rejected under the open prompt"),
                    Err(error) => error,
                };
                assert!(
                    matches!(
                        &error,
                        EngineError::InvalidAction(message) if message.contains("Priority window")
                    ),
                    "{label}: expected the Priority-window admission rejection, got {error:?}"
                );
                assert_eq!(
                    counter_rollback_set(runner),
                    before,
                    "{label}: the rejected action must leave the state unchanged"
                );
            }

            /// P0 gives P1 3 of `resource` on a Vorinclex pair (P0's doubles, P1's
            /// halves what an opponent puts). Returns the runner with the CR 616.1
            /// prompt open plus P0's and P1's Vorinclex.
            fn parked_pair_give(
                resource: Resource,
                p1_preset: u32,
                count: u32,
            ) -> (GameRunner, ObjectId, ObjectId) {
                let (scenario, p0_vorinclex, p1_vorinclex) = vorinclex_pair();
                let mut runner = debug_runner(scenario);
                resource.set(&mut runner, P1, p1_preset);
                let result = give_ok(&mut runner, P0, resource.give(P1, count));
                assert!(
                    resource.deltas(&result.events, P1).is_empty(),
                    "{}: nothing is placed before the ordering is answered",
                    resource.label()
                );
                assert_order_prompt(&runner, P1, &[p0_vorinclex, p1_vorinclex]);
                (runner, p0_vorinclex, p1_vorinclex)
            }

            /// S1: every player-counter kind and energy lands as one real
            /// placement event (CR 122.1; CR 728.1 rad; CR 107.14 energy).
            #[test]
            fn s01_every_counter_kind_and_energy_lands_as_a_placement_event() {
                let mut runner = debug_runner(scenario());
                for kind in [
                    PlayerCounterKind::Poison,
                    PlayerCounterKind::Experience,
                    PlayerCounterKind::Rad,
                    PlayerCounterKind::Ticket,
                ] {
                    let result = give_ok(&mut runner, P0, give(P0, kind, 2));
                    assert_eq!(
                        runner.state().players[0].player_counter(&kind),
                        2,
                        "{kind} counters"
                    );
                    assert_eq!(
                        count_events(&result.events, |event| matches!(
                            event,
                            GameEvent::PlayerCounterChanged { player, counter_kind, delta }
                                if *player == P0 && *counter_kind == kind && *delta == 2
                        )),
                        1,
                        "{kind}: exactly one placement event"
                    );
                }
                let result = give_ok(&mut runner, P0, give_energy(P0, 3));
                assert_eq!(Resource::Energy.get(&runner, P0), 3);
                assert_eq!(Resource::Energy.deltas(&result.events, P0), vec![3]);
                assert_priority_with_empty_stack(&runner);
            }

            /// S2, CR 122.1f + CR 104.3d + CR 704.5c + CR 704.3: the give runs the
            /// post-action pipeline, so ten poison counters lose the game.
            #[test]
            fn s02_poison_give_runs_the_state_based_actions() {
                let mut runner = debug_runner(scenario());
                runner.state_mut().players[1].poison_counters = 7;
                give_ok(&mut runner, P0, give(P1, PlayerCounterKind::Poison, 3));
                assert_eq!(poison(&runner, P1), 10);
                assert!(
                    matches!(
                        runner.state().waiting_for,
                        WaitingFor::GameOver { winner: Some(winner) } if winner == P0
                    ),
                    "ten poison counters lose the game, got {:?}",
                    runner.state().waiting_for
                );

                // Control: nine poison counters do not.
                let mut runner = debug_runner(scenario());
                runner.state_mut().players[1].poison_counters = 7;
                give_ok(&mut runner, P0, give(P1, PlayerCounterKind::Poison, 2));
                assert_eq!(poison(&runner, P1), 9);
                assert_priority_with_empty_stack(&runner);
            }

            /// S3: a zero count is accepted and is not a placement event.
            #[test]
            fn s03_zero_count_is_accepted_and_places_nothing() {
                let mut runner = debug_runner(scenario());
                let result = give_ok(&mut runner, P0, give(P0, PlayerCounterKind::Experience, 0));
                assert!(!has_counter_event(&result.events));
                let result = give_ok(&mut runner, P0, give_energy(P0, 0));
                assert!(!has_counter_event(&result.events));
                assert_eq!(Resource::Experience.get(&runner, P0), 0);
                assert_eq!(Resource::Energy.get(&runner, P0), 0);
                assert_priority_with_empty_stack(&runner);
            }

            /// S4, CR 614.17 + CR 614.17c + CR 101.2: "Players can't get counters"
            /// applies to the give, and Vorinclex cannot modify an event that can't
            /// happen (no ordering prompt).
            #[test]
            fn s04_solemnity_prevents_the_give_before_any_replacement() {
                let mut scenario = scenario();
                let solemnity = add_card(&mut scenario, P1, "Solemnity");
                add_card(&mut scenario, P0, VORINCLEX);
                let mut runner = debug_runner(scenario);
                assert_eq!(runner.state().objects[&solemnity].zone, Zone::Battlefield);

                let poison_result =
                    give_ok(&mut runner, P0, give(P0, PlayerCounterKind::Poison, 3));
                assert!(!has_counter_event(&poison_result.events));
                assert!(!replacement_prompt_open(&runner));
                let energy_result = give_ok(&mut runner, P0, give_energy(P0, 2));
                assert!(!has_counter_event(&energy_result.events));
                assert!(!replacement_prompt_open(&runner));

                assert_eq!(poison(&runner, P0), 0);
                assert_eq!(Resource::Energy.get(&runner, P0), 0);
                assert_priority_with_empty_stack(&runner);
            }

            /// S5, CR 614.1 + CR 614.6: the requester's own Vorinclex doubles the
            /// give (energy too, by the Fabrication Module 2024-06-07 ruling).
            #[test]
            fn s05_count_replacement_applies_to_the_requesters_give() {
                let mut runner = vorinclex_runner();
                let result = give_ok(&mut runner, P0, give(P0, PlayerCounterKind::Experience, 3));
                assert_eq!(Resource::Experience.get(&runner, P0), 6);
                assert_eq!(Resource::Experience.deltas(&result.events, P0), vec![6]);
                let result = give_ok(&mut runner, P0, give_energy(P0, 2));
                assert_eq!(Resource::Energy.get(&runner, P0), 4);
                assert_eq!(Resource::Energy.deltas(&result.events, P0), vec![4]);
                assert_priority_with_empty_stack(&runner);
            }

            /// S6: Doubling Season doubles counters on a permanent you control; a
            /// player is not a permanent, so the give is unchanged.
            #[test]
            fn s06_permanent_only_doubler_does_not_touch_a_player_give() {
                let mut scenario = scenario();
                let season = add_card(&mut scenario, P0, "Doubling Season");
                let mut runner = debug_runner(scenario);
                assert!(
                    runner.state().objects[&season]
                        .replacement_definitions
                        .iter_unchecked()
                        .any(|definition| definition.event == ReplacementEvent::AddCounter),
                    "reach guard: Doubling Season's counter clause must have loaded"
                );

                let result = give_ok(&mut runner, P0, give(P0, PlayerCounterKind::Experience, 3));
                assert_eq!(Resource::Experience.get(&runner, P0), 3);
                assert_eq!(Resource::Experience.deltas(&result.events, P0), vec![3]);
            }

            /// S7: the submitter is the putting player (Vorinclex ruling
            /// 2021-02-05; CR 614.1). P0's Vorinclex doubles what P0 puts and
            /// halves (rounded down) what an opponent puts, whoever receives.
            #[test]
            fn s07_submitter_is_the_putting_player() {
                let mut runner = vorinclex_runner();
                let cells = [
                    (P0, P0, 6, "P0 gives P0"),
                    (P0, P1, 6, "P0 gives P1"),
                    (P1, P0, 1, "P1 gives P0"),
                    (P1, P1, 1, "P1 gives P1"),
                ];
                for (submitter, recipient, expected_delta, label) in cells {
                    let before = Resource::Experience.get(&runner, recipient);
                    let result = give_ok(
                        &mut runner,
                        submitter,
                        give(recipient, PlayerCounterKind::Experience, 3),
                    );
                    assert_eq!(
                        Resource::Experience.deltas(&result.events, recipient),
                        vec![expected_delta],
                        "{label}"
                    );
                    assert_eq!(
                        Resource::Experience.get(&runner, recipient),
                        before + expected_delta as u32,
                        "{label}"
                    );
                }
            }

            /// S8, CR 616.1 + CR 616.1e + CR 616.1f: the affected player (the
            /// recipient, not the requester) orders the replacements, and a
            /// non-identity order applies.
            #[test]
            fn s08_recipient_orders_the_replacements_and_the_answer_applies() {
                for resource in [Resource::Experience, Resource::Energy] {
                    let label = resource.label();
                    let (mut runner, _doubler, halver_source) = parked_pair_give(resource, 0, 3);
                    assert_eq!(resource.get(&runner, P1), 0, "{label}");
                    let halver = candidate_index(&runner, halver_source);
                    // Halver first: 3 -> 1, then doubled: 2.
                    let result = runner
                        .act(GameAction::ChooseReplacement { index: halver })
                        .expect("the halver answer must apply");
                    assert_eq!(resource.get(&runner, P1), 2, "{label}");
                    assert_eq!(resource.deltas(&result.events, P1), vec![2], "{label}");
                    assert_priority_with_empty_stack(&runner);

                    // Control: doubler first: 3 -> 6, then halved: 3.
                    let (mut runner, doubler_source, _) = parked_pair_give(resource, 0, 3);
                    let doubler = candidate_index(&runner, doubler_source);
                    let result = runner
                        .act(GameAction::ChooseReplacement { index: doubler })
                        .expect("the doubler answer must apply");
                    assert_eq!(resource.get(&runner, P1), 3, "{label}");
                    assert_eq!(resource.deltas(&result.events, P1), vec![3], "{label}");
                    assert_priority_with_empty_stack(&runner);
                }
            }

            /// S9(a): under an open replacement prompt, gives (and the life
            /// sibling) are rejected before any state change, and the first event
            /// completes exactly once.
            #[test]
            fn s09a_gives_are_rejected_under_an_open_replacement_prompt() {
                let (mut runner, _, halver_source) = parked_pair_give(Resource::Experience, 0, 3);
                assert_admission_rejected(
                    &mut runner,
                    P1,
                    GameAction::Debug(DebugAction::GainLife {
                        player_id: P1,
                        amount: 1,
                    }),
                    "GainLife sibling",
                );
                assert_admission_rejected(
                    &mut runner,
                    P1,
                    give(P1, PlayerCounterKind::Experience, 1),
                    "GivePlayerCounters under the replacement prompt",
                );
                assert_admission_rejected(
                    &mut runner,
                    P0,
                    give_energy(P1, 1),
                    "GiveEnergy by P0 under the replacement prompt",
                );

                let halver = candidate_index(&runner, halver_source);
                let result = runner
                    .act(GameAction::ChooseReplacement { index: halver })
                    .expect("the original prompt must still be answerable");
                assert_eq!(Resource::Experience.get(&runner, P1), 2);
                assert_eq!(
                    Resource::Experience.deltas(&result.events, P1),
                    vec![2],
                    "the first event completes exactly once"
                );
                assert_priority_with_empty_stack(&runner);
            }

            /// S9(b): under an open non-replacement prompt a give is rejected too.
            #[test]
            fn s09b_gives_are_rejected_under_an_open_proliferate_prompt() {
                let mut runner = debug_runner(scenario());
                Resource::Experience.set(&mut runner, P0, 1);
                proliferate_to_choice(&mut runner, P0);

                assert_admission_rejected(
                    &mut runner,
                    P0,
                    give_energy(P0, 1),
                    "GiveEnergy under the proliferate prompt",
                );
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::ProliferateChoice { .. }
                ));
                runner
                    .act(GameAction::SelectTargets { targets: vec![] })
                    .expect("declining the still-open choice completes it");
                assert_priority_with_empty_stack(&runner);
            }

            /// S9(c): the `DebugAction` doc contract for a variant outside the gate:
            /// the raw energy editor "fires from any game state without disrupting
            /// the current prompt". The prompt keeps its owner and its proposal.
            #[test]
            fn s09c_raw_editor_outside_the_gate_leaves_the_prompt_with_its_owner() {
                let (mut runner, _, halver_source) = parked_pair_give(Resource::Experience, 0, 3);
                let waiting_for = runner.state().waiting_for.clone();
                let pending = runner.state().pending_replacement.clone();

                runner
                    .act(GameAction::Debug(DebugAction::ModifyEnergy {
                        player_id: P1,
                        delta: 1,
                    }))
                    .expect("the raw editor is not gated on Priority");
                assert_eq!(Resource::Energy.get(&runner, P1), 1);
                assert_eq!(runner.state().waiting_for, waiting_for);
                assert_eq!(runner.state().pending_replacement, pending);

                let halver = candidate_index(&runner, halver_source);
                let result = runner
                    .act(GameAction::ChooseReplacement { index: halver })
                    .expect("the prompt must still be answerable");
                assert_eq!(Resource::Experience.deltas(&result.events, P1), vec![2]);
            }

            /// S10: a replacement's own arithmetic past `u32` rejects synchronously
            /// with the typed error and leaves no stale record behind.
            #[test]
            fn s10_applier_overflow_rejects_and_leaves_no_stale_record() {
                for resource in [Resource::Experience, Resource::Energy] {
                    let label = resource.label();
                    let mut runner = debug_runner(scenario());
                    add_counter_quantity_source(
                        &mut runner,
                        P0,
                        QuantityModification::Times { factor: 5_000 },
                    );
                    assert_rejected_unrepresentable(
                        &mut runner,
                        P0,
                        resource.give(P0, 1_000_000),
                        P0,
                    );
                    assert_eq!(resource.get(&runner, P0), 0, "{label}");
                    // The next, representable, give on the same runner applies.
                    let result = give_ok(&mut runner, P0, resource.give(P0, 1));
                    assert_eq!(resource.get(&runner, P0), 5_000, "{label}");
                    assert_eq!(resource.deltas(&result.events, P0), vec![5_000], "{label}");

                    // Control: a representable product lands exactly.
                    let mut runner = debug_runner(scenario());
                    add_counter_quantity_source(
                        &mut runner,
                        P0,
                        QuantityModification::Times { factor: 2_000 },
                    );
                    let result = give_ok(&mut runner, P0, resource.give(P0, 1_000_000));
                    assert_eq!(resource.get(&runner, P0), 2_000_000_000, "{label}");
                    assert_eq!(
                        resource.deltas(&result.events, P0),
                        vec![2_000_000_000],
                        "{label}"
                    );
                }
            }

            /// S11: a result that fits `u32` but not an `i32` delta rejects rather
            /// than wrapping to a negative delta.
            #[test]
            fn s11_cast_overflow_rejects() {
                for resource in [Resource::Experience, Resource::Energy] {
                    let mut runner = debug_runner(scenario());
                    add_counter_quantity_source(
                        &mut runner,
                        P0,
                        QuantityModification::Times { factor: 3_000 },
                    );
                    assert_rejected_unrepresentable(
                        &mut runner,
                        P0,
                        resource.give(P0, 1_000_000),
                        P0,
                    );
                    assert_eq!(resource.get(&runner, P0), 0, "{}", resource.label());
                }
            }

            /// S12: a final total past `u32::MAX` rejects; exactly `u32::MAX` lands.
            #[test]
            fn s12_final_total_overflow_rejects() {
                for resource in [Resource::Experience, Resource::Energy] {
                    let label = resource.label();
                    let mut runner = debug_runner(scenario());
                    resource.set(&mut runner, P0, u32::MAX - 1);
                    assert_rejected_unrepresentable(&mut runner, P0, resource.give(P0, 2), P0);
                    assert_eq!(resource.get(&runner, P0), u32::MAX - 1, "{label}");

                    // Control: exactly the limit.
                    let mut runner = debug_runner(scenario());
                    resource.set(&mut runner, P0, u32::MAX - 2);
                    let result = give_ok(&mut runner, P0, resource.give(P0, 2));
                    assert_eq!(resource.get(&runner, P0), u32::MAX, "{label}");
                    assert_eq!(resource.deltas(&result.events, P0), vec![2], "{label}");
                }
            }

            /// S13: overflow after a parsed doubler (Vorinclex), at the cap; the
            /// control lands exactly on `u32::MAX` (also the cap-accepted row).
            #[test]
            fn s13_overflow_after_a_parsed_doubler_at_the_cap() {
                let mut runner = vorinclex_runner();
                Resource::Experience.set(&mut runner, P0, u32::MAX - 1_999_999);
                assert_rejected_unrepresentable(
                    &mut runner,
                    P0,
                    give(P0, PlayerCounterKind::Experience, MAX_DEBUG_COUNTER_AMOUNT),
                    P0,
                );

                let mut runner = vorinclex_runner();
                Resource::Experience.set(&mut runner, P0, u32::MAX - 2_000_000);
                let result = give_ok(
                    &mut runner,
                    P0,
                    give(P0, PlayerCounterKind::Experience, MAX_DEBUG_COUNTER_AMOUNT),
                );
                assert_eq!(Resource::Experience.get(&runner, P0), u32::MAX);
                assert_eq!(
                    Resource::Experience.deltas(&result.events, P0),
                    vec![2_000_000]
                );
            }

            /// S14: gating and recipient checks are unchanged for the new actions.
            #[test]
            fn s14_gating_and_recipient_checks() {
                // (a) Debug mode off.
                let mut runner = scenario().build();
                let before = counter_rollback_set(&runner);
                assert!(runner
                    .act(give(P0, PlayerCounterKind::Experience, 1))
                    .is_err());
                assert!(runner.act(give_energy(P0, 1)).is_err());
                assert_eq!(counter_rollback_set(&runner), before);

                // (b) Permission set that excludes the submitting seat.
                let mut runner = debug_runner(scenario());
                runner.state_mut().debug_permitted.insert(P0);
                let before = counter_rollback_set(&runner);
                let error = act_by(&mut runner, P1, give(P1, PlayerCounterKind::Experience, 1))
                    .expect_err("P1 lacks debug permission");
                assert!(error.to_string().contains("debug permission"), "{error:?}");
                assert_eq!(counter_rollback_set(&runner), before);
                give_ok(&mut runner, P0, give(P0, PlayerCounterKind::Experience, 1));

                // (c) Unknown recipient.
                let mut runner = debug_runner(scenario());
                let before = counter_rollback_set(&runner);
                assert!(runner
                    .act(give(PlayerId(9), PlayerCounterKind::Experience, 1))
                    .is_err());
                assert!(runner.act(give_energy(PlayerId(9), 1)).is_err());
                assert_eq!(counter_rollback_set(&runner), before);
                give_ok(&mut runner, P0, give(P1, PlayerCounterKind::Experience, 1));

                // (d) Eliminated recipient; a living player still receives.
                let mut three_player = GameScenario::new_n_player(3, 42);
                three_player.at_phase(crate::types::phase::Phase::PreCombatMain);
                let mut runner = debug_runner(three_player);
                let p2 = PlayerId(2);
                let mut events = Vec::new();
                crate::game::elimination::eliminate_player(runner.state_mut(), p2, &mut events);
                let before = counter_rollback_set(&runner);
                let error = runner
                    .act(give(p2, PlayerCounterKind::Experience, 1))
                    .expect_err("an eliminated recipient is rejected");
                assert!(
                    error.to_string().contains("no longer in the game"),
                    "{error:?}"
                );
                assert_eq!(counter_rollback_set(&runner), before);
                give_ok(&mut runner, P0, give(P1, PlayerCounterKind::Experience, 1));

                // (e) The cap is enforced before any state change.
                let mut runner = debug_runner(scenario());
                let over_counters = DebugAction::GivePlayerCounters {
                    player_id: P0,
                    counter_kind: PlayerCounterKind::Experience,
                    count: MAX_DEBUG_COUNTER_AMOUNT + 1,
                };
                let over_energy = DebugAction::GiveEnergy {
                    player_id: P0,
                    count: MAX_DEBUG_COUNTER_AMOUNT + 1,
                };
                assert!(preflight_debug_action(runner.state(), P0, &over_counters).is_err());
                assert!(preflight_debug_action(runner.state(), P0, &over_energy).is_err());
                let before = counter_rollback_set(&runner);
                assert!(runner.act(GameAction::Debug(over_counters)).is_err());
                assert!(runner.act(GameAction::Debug(over_energy)).is_err());
                assert_eq!(counter_rollback_set(&runner), before);
                give_ok(&mut runner, P0, give(P0, PlayerCounterKind::Experience, 1));
            }

            /// A battlefield with Fabrication Module and two Grizzly Bears (P0),
            /// optionally with Solemnity under P1.
            fn module_board(with_solemnity: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
                let mut scenario = scenario();
                let module = add_card(&mut scenario, P0, "Fabrication Module");
                let bears_a = add_card(&mut scenario, P0, "Grizzly Bears");
                let bears_b = add_card(&mut scenario, P0, "Grizzly Bears");
                if with_solemnity {
                    add_card(&mut scenario, P1, "Solemnity");
                }
                (debug_runner(scenario), module, bears_a, bears_b)
            }

            /// S15, CR 603.2 + CR 603.3 (Fabrication Module ruling 2016-09-20: one
            /// trigger per batch): the give's energy event triggers "whenever you
            /// get {E}" once; another player's gain and a prevented gain do not.
            #[test]
            fn s15_energy_give_triggers_the_module_once_per_batch() {
                let (mut runner, module, bears_a, bears_b) = module_board(false);
                give_ok(&mut runner, P0, give_energy(P0, 2));
                assert!(
                    matches!(
                        runner.state().waiting_for,
                        WaitingFor::TriggerTargetSelection { player, .. } if player == P0
                    ),
                    "the Module's target prompt must open, got {:?}",
                    runner.state().waiting_for
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(bears_b)],
                    })
                    .expect("select the second Bears");
                assert_eq!(runner.state().stack.len(), 1, "one trigger per batch");
                assert!(runner
                    .state()
                    .stack
                    .iter()
                    .all(|entry| entry.source_id == module));
                runner.advance_until_stack_empty();
                assert_eq!(p1p1(&runner, bears_b), 1);
                assert_eq!(p1p1(&runner, bears_a), 0);
                assert_priority_with_empty_stack(&runner);

                // (b) P1's energy does not match "whenever you get".
                let (mut runner, _, _, _) = module_board(false);
                let result = give_ok(&mut runner, P0, give_energy(P1, 2));
                assert_eq!(Resource::Energy.deltas(&result.events, P1), vec![2]);
                assert_priority_with_empty_stack(&runner);

                // (c) Solemnity prevents the event, so nothing triggers (CR 603.2g).
                let (mut runner, _, _, _) = module_board(true);
                let result = give_ok(&mut runner, P0, give_energy(P0, 2));
                assert!(!has_counter_event(&result.events));
                assert_eq!(Resource::Energy.get(&runner, P0), 0);
                assert_priority_with_empty_stack(&runner);
            }

            /// S16: engine-owned audit wording.
            #[test]
            fn s16_describe_strings() {
                let state = sandbox_state();
                assert_eq!(
                    DebugAction::GivePlayerCounters {
                        player_id: P1,
                        counter_kind: PlayerCounterKind::Poison,
                        count: 3,
                    }
                    .describe(&state),
                    "GivePlayerCounters (Player 2 gets 3 poison counters)"
                );
                assert_eq!(
                    DebugAction::GiveEnergy {
                        player_id: P0,
                        count: 2,
                    }
                    .describe(&state),
                    "GiveEnergy (Player 1 gets 2 energy)"
                );
            }

            /// R1, CR 616.1e: overflow after a replacement-order answer to a give's
            /// park rejects and the prompt stays answerable; the other order is
            /// representable.
            #[test]
            fn r1_overflow_after_the_order_answer_rejects_and_stays_answerable() {
                for resource in [Resource::Experience, Resource::Energy] {
                    let label = resource.label();
                    let (mut runner, p0_vorinclex, p1_vorinclex) =
                        parked_pair_give(resource, u32::MAX - 2, 3);
                    let doubler = candidate_index(&runner, p0_vorinclex);
                    let halver = candidate_index(&runner, p1_vorinclex);

                    let before = counter_rollback_set(&runner);
                    for attempt in ["first", "repeated"] {
                        let error = runner
                            .act(GameAction::ChooseReplacement { index: doubler })
                            .expect_err("the overflowing delivery must be rejected");
                        assert_unrepresentable_for(&error, P1);
                        assert_eq!(
                            counter_rollback_set(&runner),
                            before,
                            "{label}: {attempt} rejected answer"
                        );
                        assert!(replacement_prompt_open(&runner), "{label}: {attempt}");
                    }

                    let result = runner
                        .act(GameAction::ChooseReplacement { index: halver })
                        .expect("the halver-first order is representable");
                    assert_eq!(resource.get(&runner, P1), u32::MAX, "{label}");
                    assert_eq!(resource.deltas(&result.events, P1), vec![2], "{label}");
                    assert_priority_with_empty_stack(&runner);

                    // Control: one lower, doubler first (3 -> 6 -> 3) lands on the limit.
                    let (mut runner, p0_vorinclex, _) = parked_pair_give(resource, u32::MAX - 3, 3);
                    let doubler = candidate_index(&runner, p0_vorinclex);
                    let result = runner
                        .act(GameAction::ChooseReplacement { index: doubler })
                        .expect("a representable delivery must be accepted");
                    assert_eq!(resource.get(&runner, P1), u32::MAX, "{label}");
                    assert_eq!(resource.deltas(&result.events, P1), vec![3], "{label}");
                    assert_priority_with_empty_stack(&runner);
                }
            }

            /// R2, CR 616.1f: a re-parked record keeps the rejection stamp. Two
            /// halvers (P1's Vorinclex and Halving Season) and P0's doubler:
            /// 4 -> 8 -> 4 -> 2.
            #[test]
            fn r2_a_reparked_give_keeps_the_rejection_stamp() {
                let board = || {
                    let (mut scenario, p0_vorinclex, p1_vorinclex) = vorinclex_pair();
                    let season = add_card(&mut scenario, P1, "Halving Season");
                    (debug_runner(scenario), p0_vorinclex, p1_vorinclex, season)
                };
                let park = |preset: u32| {
                    let (mut runner, p0_vorinclex, p1_vorinclex, season) = board();
                    Resource::Experience.set(&mut runner, P1, preset);
                    give_ok(&mut runner, P0, give(P1, PlayerCounterKind::Experience, 4));
                    assert_order_prompt(&runner, P1, &[p0_vorinclex, p1_vorinclex, season]);
                    let doubler = candidate_index(&runner, p0_vorinclex);
                    runner
                        .act(GameAction::ChooseReplacement { index: doubler })
                        .expect("answering the doubler first is representable so far");
                    // Reach guard: the two halvers are order-material, so it re-parks.
                    assert!(replacement_prompt_open(&runner));
                    assert_eq!(
                        runner
                            .state()
                            .pending_replacement
                            .as_ref()
                            .map(|pending| pending.candidates.len()),
                        Some(2)
                    );
                    assert_eq!(parked_policy(&runner), Some(UnrepresentablePolicy::Reject));
                    runner
                };

                let mut runner = park(u32::MAX - 1);
                let before = counter_rollback_set(&runner);
                for attempt in ["first", "repeated"] {
                    let error = runner
                        .act(GameAction::ChooseReplacement { index: 0 })
                        .expect_err("the overflowing delivery must be rejected");
                    assert_unrepresentable_for(&error, P1);
                    assert_eq!(counter_rollback_set(&runner), before, "{attempt}");
                }

                // Control: one lower lands exactly on the limit.
                let mut runner = park(u32::MAX - 2);
                let result = runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("a representable delivery must be accepted");
                assert_eq!(Resource::Experience.get(&runner, P1), u32::MAX);
                assert_eq!(Resource::Experience.deltas(&result.events, P1), vec![2]);
            }

            /// R3, CR 704.3 + CR 704.5c: a parked poison give's resume runs the
            /// state-based actions.
            #[test]
            fn r3_resumed_poison_give_runs_the_state_based_actions() {
                let park = |preset: u32| {
                    let (scenario, p0_vorinclex, p1_vorinclex) = vorinclex_pair();
                    let mut runner = debug_runner(scenario);
                    runner.state_mut().players[1].poison_counters = preset;
                    give_ok(&mut runner, P0, give(P1, PlayerCounterKind::Poison, 3));
                    assert_order_prompt(&runner, P1, &[p0_vorinclex, p1_vorinclex]);
                    let halver = candidate_index(&runner, p1_vorinclex);
                    (runner, halver)
                };

                // Halver first: 3 -> 1 -> 2; 8 + 2 = 10 loses.
                let (mut runner, halver) = park(8);
                runner
                    .act(GameAction::ChooseReplacement { index: halver })
                    .expect("the halver answer must apply");
                assert_eq!(poison(&runner, P1), 10);
                assert!(
                    matches!(
                        runner.state().waiting_for,
                        WaitingFor::GameOver { winner: Some(winner) } if winner == P0
                    ),
                    "got {:?}",
                    runner.state().waiting_for
                );

                // Control: 7 + 2 = 9 does not.
                let (mut runner, halver) = park(7);
                runner
                    .act(GameAction::ChooseReplacement { index: halver })
                    .expect("the halver answer must apply");
                assert_eq!(poison(&runner, P1), 9);
                assert_priority_with_empty_stack(&runner);
            }

            /// R4, CR 603.2 + CR 603.3: a parked energy give's resume runs the
            /// recipient's "whenever you get {E}" trigger.
            #[test]
            fn r4_resumed_energy_give_runs_the_trigger() {
                let mut scenario = scenario();
                let vorinclex = add_card(&mut scenario, P0, VORINCLEX);
                let season = add_card(&mut scenario, P1, "Halving Season");
                let module = add_card(&mut scenario, P1, "Fabrication Module");
                let bears = add_card(&mut scenario, P1, "Grizzly Bears");
                let mut runner = debug_runner(scenario);

                give_ok(&mut runner, P0, give_energy(P1, 3));
                assert_order_prompt(&runner, P1, &[vorinclex, season]);
                let halving = candidate_index(&runner, season);
                // Halving Season first: 3 -> 1, then doubled: 2.
                let result = runner
                    .act(GameAction::ChooseReplacement { index: halving })
                    .expect("the Halving Season answer must apply");

                assert_eq!(Resource::Energy.get(&runner, P1), 2);
                assert_eq!(Resource::Energy.deltas(&result.events, P1), vec![2]);
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert_eq!(runner.state().stack.len(), 1, "one trigger for the batch");
                assert!(runner
                    .state()
                    .stack
                    .iter()
                    .all(|entry| entry.source_id == module));
                runner.advance_until_stack_empty();
                assert!(
                    runner.state().stack.is_empty(),
                    "the Module trigger resolves"
                );
                // The Module's controller (P1) puts the counter, so P0's Vorinclex
                // (an opponent's counters, halved, rounded down) turns 1 into 0;
                // P1's own Halving Season does not apply to P1's counters.
                assert_eq!(p1p1(&runner, bears), 0);
            }
        }
    }
}
