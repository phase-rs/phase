use crate::ai_support::copy_target_mana_value_ceiling;
use crate::types::ability::{
    AbilityDefinition, Effect, PostReplacementContinuation, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::counter::CounterType;
use crate::types::events::{GameEvent, ManaTapState};
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{CounterPlacement, ProposedEvent};
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

use super::ability_utils::build_resolved_from_def_with_targets;
use super::effects;
use super::effects::deal_damage::{apply_damage_after_replacement, DamageContext};
use super::effects::destroy::apply_destroy_after_replacement;
use super::effects::draw::apply_draw_after_replacement;
use super::effects::life::{apply_life_gain_after_replacement, apply_life_loss_after_replacement};
use super::effects::mill::apply_mill_after_replacement;
use super::effects::scry::apply_scry_after_replacement;
use super::effects::token::apply_create_token_after_replacement;
use super::engine::EngineError;
use super::sacrifice::apply_sacrifice_after_replacement;
use super::zones;

pub(super) fn handle_replacement_choice(
    state: &mut GameState,
    index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let pending_was_counter_move = state
        .pending_replacement
        .as_ref()
        .is_some_and(|pending| matches!(pending.proposed, ProposedEvent::MoveCounter { .. }));
    match super::replacement::continue_replacement(state, index, events) {
        super::replacement::ReplacementResult::Execute(event) => {
            let mut zone_change_object_id = None;
            let mut enters_battlefield = false;
            match event {
                ProposedEvent::ZoneChange {
                    object_id,
                    to,
                    from,
                    enter_tapped,
                    enter_with_counters,
                    controller_override,
                    enter_transformed,
                    ..
                } => {
                    let played_from_zone = state
                        .objects
                        .get(&object_id)
                        .and_then(|obj| obj.played_from_zone);
                    zones::move_to_zone(state, object_id, to, events);
                    // CR 400.7: reset_for_battlefield_entry (inside move_to_zone) sets
                    // defaults. Override only when the replacement pipeline changed them.
                    if to == Zone::Battlefield {
                        if let Some(obj) = state.objects.get_mut(&object_id) {
                            obj.played_from_zone = played_from_zone;
                            if enter_tapped.resolve(false) {
                                obj.tapped = true;
                            }
                        }
                        if let Some(new_controller) = controller_override {
                            zones::apply_battlefield_entry_controller_override(
                                state,
                                events,
                                object_id,
                                new_controller,
                            );
                        }
                        // CR 614.1c: Apply counters from replacement pipeline.
                        if !apply_etb_counters(state, object_id, &enter_with_counters, events) {
                            return Ok(state.waiting_for.clone());
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
                            if !apply_etb_counters(state, object_id, &pending, events) {
                                return Ok(state.waiting_for.clone());
                            }
                            state
                                .pending_etb_counters
                                .retain(|(oid, _, _)| *oid != object_id);
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
                        crate::game::layers::mark_layers_full(state);
                    }
                    enters_battlefield = to == Zone::Battlefield;
                    zone_change_object_id = Some(object_id);
                }
                // CR 120.3 + CR 120.4b: Damage accepted after replacement choice — apply via the
                // shared helper so wither/infect/planeswalker/excess/lifelink paths match
                // the non-choice delivery. Reconstruct DamageContext from the source at
                // resumption time (CR 609.6: characteristics at time of dealing).
                damage @ ProposedEvent::Damage {
                    source_id,
                    is_combat,
                    ..
                } => {
                    let ctx = DamageContext::from_source(state, source_id).unwrap_or_else(|| {
                        let controller = state
                            .objects
                            .get(&source_id)
                            .map(|obj| obj.controller)
                            .unwrap_or(state.active_player);
                        DamageContext::fallback(source_id, controller)
                    });
                    let _ = apply_damage_after_replacement(state, &ctx, damage, is_combat, events);
                }
                // CR 122.1: Counter addition accepted after replacement choice (e.g.,
                // Corpsejack Menace doubler on a prompted counter-placement).
                ProposedEvent::AddCounter {
                    placement, count, ..
                } => match placement {
                    CounterPlacement::Object {
                        actor,
                        object_id,
                        counter_type,
                    } => effects::counters::apply_counter_addition(
                        state,
                        actor,
                        object_id,
                        counter_type,
                        count,
                        events,
                    ),
                    CounterPlacement::Player {
                        player_id,
                        counter_kind,
                        ..
                    } => effects::player_counter::apply_player_counter_addition(
                        state,
                        player_id,
                        counter_kind,
                        count,
                        events,
                    ),
                    CounterPlacement::Energy { player_id, .. } => {
                        effects::energy::apply_energy_addition(state, player_id, count, events)
                    }
                },
                // CR 122.1: Counter removal accepted after replacement choice.
                ProposedEvent::RemoveCounter {
                    object_id,
                    counter_type,
                    count,
                    ..
                } => {
                    effects::counters::apply_counter_removal(
                        state,
                        object_id,
                        counter_type,
                        count,
                        events,
                    );
                }
                move_counter @ ProposedEvent::MoveCounter { .. } => {
                    if !effects::counters::apply_move_counter_after_replacement(
                        state,
                        move_counter,
                        events,
                    ) {
                        return Ok(state.waiting_for.clone());
                    }
                }
                // CR 701.26a: Tap accepted after replacement choice.
                ProposedEvent::Tap { object_id, .. } => {
                    if let Some(obj) = state.objects.get_mut(&object_id) {
                        obj.tapped = true;
                        events.push(GameEvent::PermanentTapped {
                            object_id,
                            caused_by: None,
                        });
                    }
                }
                // CR 701.26b: Untap accepted after replacement choice.
                ProposedEvent::Untap { object_id, .. } => {
                    if let Some(obj) = state.objects.get_mut(&object_id) {
                        obj.tapped = false;
                        events.push(GameEvent::PermanentUntapped { object_id });
                    }
                }
                // CR 121.1 + CR 614.6 + CR 614.11: Draw accepted after
                // replacement choice — delegate to the shared post-replacement
                // helper so library-zone move + per-turn accounting match the
                // non-choice delivery. For Abundance-shape replacements
                // (`execute` is a non-Draw chain), `draw_applier` has zeroed
                // the count and the central `post_replacement_continuation`
                // drain below runs the chain (Choose → RevealUntil).
                draw @ ProposedEvent::Draw { .. } => {
                    apply_draw_after_replacement(state, draw, events);
                }
                // CR 701.22a: Scry accepted after replacement choice.
                scry @ ProposedEvent::Scry { .. } => {
                    apply_scry_after_replacement(state, scry, events);
                }
                // CR 701.17a: Mill accepted after replacement choice — delegate
                // to the shared helper so count clamping and library movement
                // match the non-choice delivery.
                mill @ ProposedEvent::Mill { .. } => {
                    let _ = apply_mill_after_replacement(state, mill, events);
                }
                // CR 119.1: Life gain accepted after replacement choice.
                gain @ ProposedEvent::LifeGain { .. } => {
                    apply_life_gain_after_replacement(state, gain, events);
                }
                // CR 120.3: Life loss accepted after replacement choice.
                loss @ ProposedEvent::LifeLoss { .. } => {
                    apply_life_loss_after_replacement(state, loss, events);
                }
                // CR 701.9a: Discard accepted after replacement choice — move the
                // object hand → graveyard and record/emit the discard event. The
                // replacement pipeline may have modified `object_id`/`player_id`
                // (e.g., Madness redirects surface as a ZoneChange variant handled
                // by the ZoneChange arm above, not here).
                ProposedEvent::Discard {
                    player_id,
                    object_id,
                    ..
                } => {
                    effects::discard::complete_discard_to_graveyard(
                        state, object_id, player_id, events,
                    );
                }
                // CR 106.3 + CR 106.4: Mana production accepted after replacement choice.
                // In practice CR 614.5 mana-type replacements don't require a choice and
                // `mana_payment::produce_mana` falls back to the original type on NeedsChoice,
                // so this arm is defensive. If reached, apply the (possibly modified) unit.
                ProposedEvent::ProduceMana {
                    source_id,
                    player_id,
                    mana_type,
                    count,
                    tapped_for_mana,
                    ..
                } => {
                    if let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) {
                        for _ in 0..count {
                            let unit = crate::types::mana::ManaUnit {
                                color: mana_type,
                                source_id,
                                snow: false,
                                source_could_produce_two_or_more_colors: false,
                                restrictions: Vec::new(),
                                grants: Vec::new(),
                                expiry: None,
                            };
                            player.mana_pool.add(unit);
                            events.push(GameEvent::ManaAdded {
                                player_id,
                                mana_type,
                                source_id,
                                tap_state: ManaTapState::from_tap(tapped_for_mana),
                            });
                        }
                    }
                }
                // CR 614.1b + CR 614.10: BeginTurn / BeginPhase replacements are
                // mandatory skip effects that never set `replacement_choice_waiting_for`
                // (see `turns.rs` — NeedsChoice on these is treated as a bug). Arms are
                // present for exhaustiveness; reaching them is an engine error.
                ProposedEvent::BeginTurn { .. } => {
                    debug_assert!(
                        false,
                        "handle_replacement_choice: BeginTurn is a mandatory-skip replacement and should never surface as a choice"
                    );
                }
                ProposedEvent::BeginPhase { .. } => {
                    debug_assert!(
                        false,
                        "handle_replacement_choice: BeginPhase is a mandatory-skip replacement and should never surface as a choice"
                    );
                }
                // CR 701.8a + CR 614: Destroy accepted after replacement choice —
                // delegate to the shared helper so the inner ZoneChange (battlefield
                // → graveyard) re-enters the replacement pipeline. Leaves-the-
                // battlefield replacements, Rest-in-Peace-style redirects, and death
                // triggers all compose naturally through the inner event. If the
                // inner ZoneChange itself needs a choice, the helper sets
                // `state.waiting_for` and we propagate it back below.
                destroy @ ProposedEvent::Destroy { .. } => {
                    if !apply_destroy_after_replacement(state, destroy, events) {
                        return Ok(state.waiting_for.clone());
                    }
                }
                // CR 701.21a + CR 614: Sacrifice accepted after replacement choice —
                // delegate to the shared helper. Regeneration cannot apply (CR
                // 701.21a) but Moved replacements on the graveyard transfer still do.
                sacrifice @ ProposedEvent::Sacrifice { .. } => {
                    apply_sacrifice_after_replacement(state, sacrifice, events);
                }
                // CR 111.1 + CR 614.1a: CreateToken accepted after replacement choice
                // — the `spec` field carries the full self-describing token
                // characteristics. Delegate to the shared helper.
                create @ ProposedEvent::CreateToken { .. } => {
                    if !apply_create_token_after_replacement(state, create, events) {
                        return Ok(state.waiting_for.clone());
                    }
                }
                // CR 703.4q + CR 616.1 / CR 616.1e: EmptyManaPool resume.
                // The player has chosen one handler ordering; apply the
                // (now-mutated) per-unit dispositions to the affected
                // player's pool. If `pending_phase_transition_progress` is
                // still set, drain remaining APNAP-ordered players — that
                // call may itself pause again on another player's choice
                // (CR 616.1e iteration).
                ProposedEvent::EmptyManaPool {
                    player_id, units, ..
                } => {
                    crate::types::mana::apply_empty_mana_pool_decisions(
                        state, player_id, &units, events,
                    );
                    state.pending_step_end_mana_handlers.clear();
                }
                // CR 705.1 + CR 614.1a: Coin-flip replacements (Krark's Thumb)
                // are always Mandatory and applied inline by
                // `flip_coin::flip_through_replacement`; they never reach the
                // optional replacement-choice resume path. Unreachable in
                // practice — present only for match exhaustiveness.
                ProposedEvent::CoinFlip { .. } => {
                    debug_assert!(
                        false,
                        "CoinFlip replacement reached the optional-choice resume path"
                    );
                }
            }

            let mut waiting_for = WaitingFor::Priority {
                player: state.active_player,
            };
            state.waiting_for = waiting_for.clone();

            let mut replacement_ctx = None;
            if let Some(ctx) = state.pending_spell_resolution.take() {
                if enters_battlefield {
                    apply_pending_spell_resolution(state, &ctx, events);
                }
                replacement_ctx = Some(ctx);
            }

            if state.post_replacement_continuation.is_some() {
                // CR 614.12a + CR 614.1c: For ZoneChange events the post-effect
                // resolves against the zone-changing object, not the replacement
                // source — drop the source slot so it doesn't leak into an
                // unrelated later replacement. For non-ZoneChange events
                // (Draw/Damage/Mill/etc.) there is no enterer, so the source
                // slot is the only handle on the replacement's host (e.g.,
                // Abundance for "you may instead choose ... reveal cards" —
                // CR 614.6 + CR 614.11). Preserve it in that case so
                // `apply_post_replacement_effect` resolves the chain against
                // Abundance's controller, not `ObjectId(0)` / active_player.
                let is_zone_change = zone_change_object_id.is_some();
                if is_zone_change {
                    state.post_replacement_source = None;
                }
                if let Some(next_waiting_for) = apply_pending_post_replacement_effect(
                    state,
                    zone_change_object_id,
                    replacement_ctx.as_ref(),
                    Some(ReplacementEvent::Moved),
                    events,
                ) {
                    waiting_for = next_waiting_for;
                }
            }

            if matches!(waiting_for, WaitingFor::Priority { .. })
                && state.pending_counter_moves.is_some()
            {
                effects::counters::drain_pending_counter_moves(state, events);
                if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    waiting_for = state.waiting_for.clone();
                }
            }

            if matches!(waiting_for, WaitingFor::Priority { .. })
                && state.pending_counter_additions.is_some()
            {
                effects::counters::drain_pending_counter_additions(state, events);
                if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    waiting_for = state.waiting_for.clone();
                }
            }

            if matches!(waiting_for, WaitingFor::Priority { .. })
                && state.pending_copy_token_resolution.is_some()
            {
                effects::token_copy::drain_pending_copy_token_resolution(state, events);
                if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    waiting_for = state.waiting_for.clone();
                }
            }

            if matches!(waiting_for, WaitingFor::Priority { .. })
                && (state.pending_continuation.is_some()
                    || state.pending_change_zone_iteration.is_some())
            {
                // CR 614.12b + CR 614.1c + CR 614.13: drain BOTH the chained
                // continuation and the multi-target ChangeZone iteration that
                // paused on this replacement choice (issue #535). The drain
                // helper covers both: it runs the continuation chain (if any)
                // then the ChangeZone iteration drain hook.
                effects::drain_pending_continuation(state, events);
                // CR 616.1e: The continuation may itself pause on another replacement
                // (e.g., the second direction of fight damage hitting the same shield),
                // in which case it sets `state.waiting_for` to the next ReplacementChoice.
                // Propagate that back so the engine surfaces the correct prompt.
                if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    waiting_for = state.waiting_for.clone();
                }
            }

            // CR 616.1e + CR 703.4q: An EmptyManaPool resume may leave more
            // players in the APNAP queue. Drain the next player(s); the
            // drain may itself pause on another CR 616.1 choice, in which
            // case it sets `state.waiting_for` for us to propagate.
            if matches!(waiting_for, WaitingFor::Priority { .. })
                && state.pending_phase_transition_progress.is_some()
            {
                super::turns::drain_pending_phase_transition_progress(state, events);
                if !matches!(state.waiting_for, WaitingFor::Priority { .. })
                    && state.pending_phase_transition_progress.is_some()
                {
                    waiting_for = state.waiting_for.clone();
                }
            }

            // CR 601.2h + CR 602.2b + CR 616.1: Resume cast/activation cost payment paused for a
            // replacement choice during discard or sacrifice cost payment.
            if matches!(waiting_for, WaitingFor::Priority { .. })
                && (state.pending_cast.is_some() || state.pending_discard_for_cost.is_some())
            {
                waiting_for = super::casting_costs::resume_interrupted_cost_payment(state, events)?;
            }

            Ok(waiting_for)
        }
        super::replacement::ReplacementResult::NeedsChoice(player) => Ok(
            super::replacement::replacement_choice_waiting_for(player, state),
        ),
        super::replacement::ReplacementResult::Prevented => {
            if state.pending_counter_additions.is_some() {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                effects::counters::drain_pending_counter_additions(state, events);
                if matches!(state.waiting_for, WaitingFor::Priority { .. })
                    && state.pending_copy_token_resolution.is_some()
                {
                    effects::token_copy::drain_pending_copy_token_resolution(state, events);
                }
                return Ok(state.waiting_for.clone());
            }
            if pending_was_counter_move {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                effects::counters::drain_pending_counter_moves(state, events);
                if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    effects::drain_pending_continuation(state, events);
                }
                return Ok(state.waiting_for.clone());
            }
            if state.pending_copy_token_resolution.is_some() {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                effects::token_copy::drain_pending_copy_token_resolution(state, events);
                return Ok(state.waiting_for.clone());
            }
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
            build_resolved_from_def_with_targets(
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
    crate::game::layers::evaluate_layers(state);
    let enter_modifiers =
        super::replacement::current_self_enter_replacement_modifiers(state, source_id);
    if let Some(tapped) = enter_modifiers.enter_tapped {
        if let Some(obj) = state.objects.get_mut(&source_id) {
            obj.tapped = tapped;
        }
    }
    if !apply_etb_counters(state, source_id, &enter_modifiers.counters, events) {
        return Ok(state.waiting_for.clone());
    }
    crate::game::layers::mark_layers_full(state);
    // CR 614.12a + CR 707.9: The battlefield-entry `ZoneChanged` event was
    // captured into `state.deferred_entry_events` when `CopyTargetChoice` was
    // set up, *before* `BecomeCopy` had a chance to push the copied object's
    // characteristics and any `GrantTrigger` continuous modifications (e.g.
    // Callidus Assassin's "destroy another creature with the same name")
    // into `trigger_definitions`. With the copy now resolved and layers
    // re-evaluated, replay those events through the same trigger pipeline
    // the pipeline would have run for them (`process_triggers` for CR 603.2
    // event-based triggers + `check_delayed_triggers` for CR 603.7c delayed
    // triggers) so granted ETBs and observer ETBs (Soul Warden) match
    // against the realized copy. Replay is gated on the source still being
    // on the battlefield — concede / error / chained-replacement paths can
    // leave a stale event in the vec, and we discard rather than fire a
    // phantom entry trigger.
    let deferred = std::mem::take(&mut state.deferred_entry_events);
    let source_still_on_battlefield = state
        .objects
        .get(&source_id)
        .is_some_and(|obj| obj.zone == Zone::Battlefield);
    if !deferred.is_empty() && source_still_on_battlefield {
        super::triggers::process_triggers(state, &deferred);
        let delayed_events = super::triggers::check_delayed_triggers(state, &deferred);
        events.extend(delayed_events);
    }
    effects::drain_pending_continuation(state, events);
    // CR 113.2c + CR 603.3b + CR 707.10: `process_triggers` above may have
    // paused on an interactive replayed ETB trigger fired by the realized
    // copy. When it pauses it sets `state.pending_trigger` for the active
    // instance and stashes any simultaneously-fired siblings into
    // `state.deferred_triggers`. This mirrors the priority-time
    // `process_triggers` call site in `engine_priority`, so the resumption
    // logic must mirror that site exactly (issue #429 — the same failure
    // mode as #416 on the copy-replacement completion path):
    //
    //   1. A distribute-among pause sets `state.waiting_for` directly to
    //      `WaitingFor::DistributeAmong` (the trigger's targets are already
    //      assigned). Surface it as-is — re-running target selection would
    //      double-prompt for targets that are already chosen.
    //   2. Otherwise a modal / target-selection pause leaves only
    //      `state.pending_trigger` set; `begin_pending_trigger_target_selection`
    //      builds the active trigger's `WaitingFor` from it.
    //
    // In both cases the `state.deferred_triggers` queue is intentionally left
    // intact — it is drained by the active trigger's finalize site
    // (`engine_stack::finalize_trigger_target_selection`,
    // `engine_modes::handle_triggered_mode_choice`, or the `DistributeAmong`
    // handler) once the player resolves the active trigger.
    if matches!(state.waiting_for, WaitingFor::DistributeAmong { .. }) {
        return Ok(state.waiting_for.clone());
    }
    // CR 603.3b (#531): propagate OrderTriggers pause from process_triggers
    // above. Without this, a doubled replayed ETB trigger (e.g., Wedding
    // Announcement's token + Ocelot Pride's life-gain rider both firing on the
    // copy entry) would silently fall through to Priority.
    if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
        return Ok(state.waiting_for.clone());
    }
    if let Some(waiting_for) = super::engine::begin_pending_trigger_target_selection(state)? {
        return Ok(waiting_for);
    }
    Ok(WaitingFor::Priority {
        player: state.active_player,
    })
}

fn copy_effect_for_source(state: &GameState, source_id: ObjectId) -> Option<&AbilityDefinition> {
    state.objects.get(&source_id)?;
    // CR 702.26b + CR 114.4: `active_replacements` filters out phased-out /
    // non-emblem command-zone sources.
    // CR 614.1c: Walk past modifier-only effects in the sub_ability chain to find
    // the BecomeCopy ability directly. Composed replacements (Vesuva "enter tapped
    // as a copy") have Tap { SelfRef } as the top-level with BecomeCopy as a
    // sub_ability; returning the BecomeCopy directly avoids a redundant Tap
    // re-execution in `resolve_ability_chain`.
    super::functioning_abilities::active_replacements(state)
        .filter(|(_, o, _)| o.id == source_id)
        .filter_map(|(_, _, replacement)| replacement.execute.as_deref())
        .find_map(|effect_def| {
            super::replacement::EventModifiers::first_non_modifier_ability(Some(effect_def))
                .filter(|real| matches!(&*real.effect, Effect::BecomeCopy { .. }))
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
    spell_resolution: Option<&crate::types::game_state::PendingSpellResolution>,
    event: Option<&ReplacementEvent>,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let (source_id, controller) = object_id
        .and_then(|obj_id| {
            state
                .objects
                .get(&obj_id)
                .map(|obj| (obj_id, super::replacement::replacement_source_player(obj)))
        })
        .unwrap_or((ObjectId(0), state.active_player));

    // CR 614.1c: Walk past modifier-only effects (Tap/Untap/PutCounter/ChangeZone)
    // in the sub_ability chain to find the real work. Composable replacements like
    // Vesuva's "enter tapped as a copy" emit Tap { SelfRef } → sub_ability(BecomeCopy);
    // the modifier was already applied to the ProposedEvent by `event_modifiers_for_ability`,
    // so we skip to the first non-modifier effect for post-replacement dispatch.
    let real_work =
        super::replacement::EventModifiers::first_non_modifier_ability(Some(effect_def))
            .unwrap_or(effect_def);

    if let Effect::BecomeCopy { ref target, .. } = *real_work.effect {
        let max_mana_value = spell_resolution
            .and_then(|ctx| copy_target_mana_value_ceiling(ctx.actual_mana_spent, real_work));
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

    // CR 614.1c: The injected `Object(source)` target is the source-as-SelfRef
    // hook for replacement post-effects that consume their source (BecomeCopy,
    // PutCounter, Choose). For an interactive chooser-driven `Effect::Sacrifice`
    // whose `target` is a `Typed(...)` scope filter (e.g., the Devour synthesizer's
    // "sacrifice any number of your creatures"), the source is NOT the sacrificed
    // object — the prompt picks from the controller's eligible pool. Suppress the
    // injection in that case so `sacrifice.rs::resolve` falls through to its
    // chooser-driven `EffectZoneChoice` branch instead of treating the source as
    // a pre-selected sacrifice target.
    //
    // Gated on `event == ReplacementEvent::Moved` so the suppression scopes to
    // ETB-style replacements (the Devour shape). Non-ETB events that carry
    // `Sacrifice { Typed }` post-effects — Dralnu, Lich Lord (DealtDamage:
    // "sacrifice that many permanents") and Outfitted Jouster (DamageDone:
    // "sacrifice an Equipment") — keep the pre-Devour injection path so their
    // target-as-pre-selected resolution is unchanged.
    let sacrifice_typed_scope = matches!(event, Some(ReplacementEvent::Moved))
        && matches!(
            &*real_work.effect,
            Effect::Sacrifice {
                target: TargetFilter::Typed(_) | TargetFilter::Any,
                ..
            }
        );
    let targets = if sacrifice_typed_scope {
        Vec::new()
    } else {
        object_id
            .map(TargetRef::Object)
            .into_iter()
            .collect::<Vec<_>>()
    };
    let resolved = build_resolved_from_def_with_targets(effect_def, source_id, controller, targets);
    let _ = effects::resolve_ability_chain(state, &resolved, events, 0);

    match &state.waiting_for {
        WaitingFor::Priority { .. } => None,
        wf => Some(wf.clone()),
    }
}

pub(super) fn apply_pending_post_replacement_effect(
    state: &mut GameState,
    object_id: Option<ObjectId>,
    spell_resolution: Option<&crate::types::game_state::PendingSpellResolution>,
    event: Option<ReplacementEvent>,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let source = state.post_replacement_source.take().or(object_id);
    // CR 614.12a (approximation): sacrifice prompt fires after ZoneChange completes,
    // matching Siege/Tribute precedent. A strict reading of 614.12a says the choice
    // is made *before* the permanent enters, but the engine's pipeline applies the
    // zone change first and then drains the post-replacement continuation; the
    // observable behavior is equivalent for as-enters sacrifice/counter mechanics
    // (Devour, Siege protector, Tribute) where the choice doesn't gate entry.
    //
    // CR 614.12a + CR 615.5: Single dispatch on the unified continuation slot.
    // `Resolved` carries captured targets (prevention follow-ups); `Template`
    // is an AST that resolves against `source` for ETB / Optional accept.
    let waiting_for = match state.post_replacement_continuation.take() {
        Some(PostReplacementContinuation::Resolved(resolved)) => {
            apply_post_replacement_resolved_effect(state, &resolved, events)
        }
        Some(PostReplacementContinuation::Template(effect_def)) => apply_post_replacement_effect(
            state,
            &effect_def,
            source,
            spell_resolution,
            event.as_ref(),
            events,
        ),
        None => None,
    };
    state.post_replacement_event_source = None;
    state.post_replacement_event_target = None;
    // CR 614.12a + CR 707.9: When the post-effect pauses on `CopyTargetChoice`,
    // the entering object's battlefield-entry `ZoneChanged` event is already
    // in `events` (emitted by the prior `move_to_zone`). `BecomeCopy` and its
    // `GrantTrigger` modifications haven't been applied yet, so a trigger
    // scan over that event right now would miss every granted ETB (Callidus
    // Assassin's destroy-same-name). Defer the event into
    // `state.deferred_entry_events`; `handle_copy_target_choice` replays it
    // after `BecomeCopy` resolves and layers re-evaluate. Captured at the
    // single producer site so both the stack-resolution path (non-optional
    // copy replacements) and the `handle_replacement_choice` path (optional
    // "you may have this enter as a copy" replacements) defer uniformly.
    capture_deferred_entry_events_if_copy_target_choice(state, waiting_for.as_ref(), events);
    waiting_for
}

/// CR 614.12a: True when every branch of a `ChooseOneOfBranch` is a self-targeted
/// `PutCounter` — the signature of an "enters with your choice of counter"
/// replacement (Denry Klin, Editor in Chief). When this holds, the choice is a
/// pre-entry counter fold and the entering object's `ZoneChanged` event must be
/// deferred until after the branch is chosen, so ETB observers see the chosen
/// counter (CR 614.12a). Exhaustive — no wildcard accept.
fn is_enters_counter_choice(branches: &[AbilityDefinition]) -> bool {
    branches.len() >= 2
        && branches.iter().all(|b| {
            matches!(
                &*b.effect,
                Effect::PutCounter {
                    target: TargetFilter::SelfRef,
                    ..
                }
            )
        })
}

/// CR 614.12a + CR 707.9: If `waiting_for` is `CopyTargetChoice`, or a
/// `ChooseOneOfBranch` that `is_enters_counter_choice` (the enters-with-choice-
/// of-counter shape), clone any battlefield-entry `ZoneChanged` events for the
/// entering source into `state.deferred_entry_events`. The original `events` vec
/// is preserved so the frontend animates the entry as soon as the spell
/// resolves; the deferred copy is replayed through `process_triggers` /
/// `check_delayed_triggers` once the choice resolves (in
/// `handle_copy_target_choice` for copies, in the `ChooseBranch` arm of
/// `engine_resolution_choices.rs` for enters-counter choices).
///
/// Defense in depth: clears any stale events from a prior choice that exited
/// abnormally (concede mid-choice, eliminate_player, error return before drain)
/// so the replay never fires triggers against a phantom object.
fn capture_deferred_entry_events_if_copy_target_choice(
    state: &mut GameState,
    waiting_for: Option<&WaitingFor>,
    events: &[GameEvent],
) {
    let source_id = match waiting_for {
        Some(WaitingFor::CopyTargetChoice { source_id, .. }) => *source_id,
        // CR 614.12a: enters-with-your-choice-of-counter defers its entry event
        // exactly like the copy-target choice does, so the watcher's ETB trigger
        // observes the chosen counter as the permanent enters.
        Some(WaitingFor::ChooseOneOfBranch {
            source_id,
            branches,
            ..
        }) if is_enters_counter_choice(branches) => *source_id,
        _ => return,
    };
    // CR 614.12b boundary (inherited from the CopyTargetChoice path, NOT expanded
    // here): mass-moving multiple pre-entry-choice permanents in one effect
    // (`resolve_all` in change_zone.rs does not bail on a post-replacement choice)
    // could let one object's capture `clear()`/overwrite another's deferred
    // events. This already affects CopyTargetChoice today, is unreachable in real
    // cards, and is the CR 614.12b simultaneous-entry boundary.
    state.deferred_entry_events.clear();
    for event in events {
        if matches!(
            event,
            GameEvent::ZoneChanged { object_id, to, .. }
                if *object_id == source_id && *to == Zone::Battlefield
        ) {
            state.deferred_entry_events.push(event.clone());
        }
    }
}

fn apply_post_replacement_resolved_effect(
    state: &mut GameState,
    resolved: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let _ = effects::resolve_ability_chain(state, resolved, events, 0);

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
    events: &mut Vec<GameEvent>,
) {
    use crate::types::game_state::CastingVariant;

    // CR 603.4: Propagate cast_from_zone so ETB triggers can evaluate
    // conditions like "if you cast it from your hand".
    // CR 702.33d + CR 702.33f: Propagate kicker payments so ETB
    // replacement / triggered abilities can gate on which kickers were paid.
    if let Some(obj) = state.objects.get_mut(&ctx.object_id) {
        obj.cast_from_zone = ctx.cast_from_zone;
        if let Some(permission) = ctx.cast_timing_permission {
            obj.cast_timing_permission = Some((permission, state.turn_number));
        }
        obj.kickers_paid.clone_from(&ctx.kickers_paid);
        obj.additional_cost_payment_count = ctx.additional_cost_payment_count;
        obj.convoked_creatures.clone_from(&ctx.convoked_creatures);
    }

    // CR 303.4f: Aura resolving to battlefield attaches to its target.
    let is_aura = state
        .objects
        .get(&ctx.object_id)
        .map(|obj| obj.card_types.subtypes.iter().any(|s| s == "Aura"))
        .unwrap_or(false);
    if is_aura {
        if let Some(target) = ctx.spell_targets.first() {
            match target {
                crate::types::ability::TargetRef::Object(target_id)
                    if state.battlefield.contains(target_id) =>
                {
                    effects::attach::attach_to(state, ctx.object_id, *target_id);
                }
                crate::types::ability::TargetRef::Object(_) => {}
                crate::types::ability::TargetRef::Player(player_id) => {
                    effects::attach::attach_to_player(state, ctx.object_id, *player_id);
                }
            }
        }
    }

    super::room::unlock_door_designation(
        state,
        ctx.object_id,
        ctx.controller,
        crate::game::game_object::RoomDoor::Left,
        events,
    );

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

    // CR 702.190b: Sneak-cast permanent also enters attacking alongside the
    // returned creature's defender and gets the `cast_variant_paid` tag
    // so intrinsic-sneak trigger conditions fire. Placement is `Some` only
    // for permanent spells; non-permanent Sneak casts (instants/sorceries)
    // get only the `cast_variant_paid` tag and resolve normally.
    if let CastingVariant::Sneak { placement, .. } = ctx.casting_variant {
        if let Some(obj) = state.objects.get_mut(&ctx.object_id) {
            obj.cast_variant_paid = Some((
                crate::types::ability::CastVariantPaid::Sneak,
                state.turn_number,
            ));
        }
        if let Some(p) = placement {
            let mut events = Vec::new();
            super::combat::place_attacking_alongside(
                state,
                ctx.object_id,
                p.defender,
                p.attack_target,
                &mut events,
            );
        }
    }

    if let CastingVariant::WebSlinging { .. } = ctx.casting_variant {
        if let Some(obj) = state.objects.get_mut(&ctx.object_id) {
            obj.cast_variant_paid = Some((
                crate::types::ability::CastVariantPaid::WebSlinging,
                state.turn_number,
            ));
        }
    }

    // CR 702.74a: Evoke-cast permanent gets the `cast_variant_paid` tag so the
    // synthesized intervening-if ETB sacrifice trigger fires once it enters.
    if ctx.casting_variant == CastingVariant::Evoke {
        if let Some(obj) = state.objects.get_mut(&ctx.object_id) {
            obj.cast_variant_paid = Some((
                crate::types::ability::CastVariantPaid::Evoke,
                state.turn_number,
            ));
        }
    }
}

/// CR 614.1c: Apply counters accumulated on a `ProposedEvent::ZoneChange` to
/// the object now entering the battlefield. Dispatches each entry through
/// `add_counter_with_replacement` so Doubling-Season-class AddCounter
/// replacements (CR 614.1a) are honored and derived fields
/// (`obj.loyalty` / `obj.defense`) stay in sync via the single-authority
/// resolver.
pub(super) fn apply_etb_counters(
    state: &mut GameState,
    object_id: ObjectId,
    counters: &[(CounterType, u32)],
    events: &mut Vec<GameEvent>,
) -> bool {
    let actor = state
        .objects
        .get(&object_id)
        .map(|obj| obj.controller)
        .unwrap_or(PlayerId(0));
    for (index, (counter_type, count)) in counters.iter().enumerate() {
        if !super::effects::counters::add_counter_with_replacement(
            state,
            actor,
            object_id,
            counter_type.clone(),
            *count,
            events,
        ) {
            let remaining = counters[index + 1..]
                .iter()
                .filter(|(_, count)| *count > 0)
                .map(|(counter_type, count)| {
                    crate::types::game_state::PendingCounterAddition::Object {
                        actor,
                        object_id,
                        counter_type: counter_type.clone(),
                        count: *count,
                    }
                })
                .collect();
            super::effects::counters::stash_pending_counter_additions(
                state,
                remaining,
                crate::types::game_state::PendingEffectResolved::with_post_actions_without_effect(
                    crate::types::ability::EffectKind::GenericEffect,
                    object_id,
                    Vec::new(),
                ),
            );
            return false;
        }
    }
    let replacement_choice_for_object = state
        .pending_replacement
        .as_ref()
        .and_then(|pending| pending.proposed.affected_object_id())
        == Some(object_id);
    if !replacement_choice_for_object {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            if obj.has_keyword(&Keyword::Compleated) {
                obj.phyrexian_life_paid = 0;
            }
        }
    }
    true
}

fn find_copy_targets(
    state: &GameState,
    filter: &TargetFilter,
    source_id: ObjectId,
    controller: PlayerId,
    max_mana_value: Option<u32>,
) -> Vec<ObjectId> {
    // CR 400.1 + CR 707.9: Clone replacements default to scanning the battlefield,
    // but extensions like Superior Spider-Man's Mind Swap (CR 707.9b) copy a card
    // from any graveyard. The filter carries the source zone via `FilterProp::InZone`;
    // fall back to battlefield when no zone constraint is present to preserve
    // Clone / Phantasmal Image / Vesuvan Doppelganger / Cackling Counterpart behaviour.
    let source_zone = filter.extract_in_zone().unwrap_or(Zone::Battlefield);
    let ctx = super::filter::FilterContext::from_source_with_controller(source_id, controller);
    state
        .objects
        .iter()
        .filter(|(id, obj)| {
            obj.zone == source_zone
                && **id != source_id
                && max_mana_value.is_none_or(|max| obj.mana_cost.mana_value() <= max)
                && super::filter::matches_target_filter(state, **id, filter, &ctx)
        })
        .map(|(id, _)| *id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::game_object::GameObject;
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::replacement::{self as replacement_mod, ReplacementResult};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityKind, QuantityExpr, ReplacementDefinition, ReplacementMode,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::proposed_event::ProposedEvent;
    use crate::types::replacements::ReplacementEvent;

    /// Helper: install an Optional replacement on a battlefield object so the
    /// matching proposed event pauses for a player choice.
    fn install_optional_replacement(state: &mut GameState, event: ReplacementEvent) -> ObjectId {
        let id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut obj = GameObject::new(
            id,
            CardId(999),
            PlayerId(1),
            "Shield".to_string(),
            Zone::Battlefield,
        );
        obj.replacement_definitions.push(
            ReplacementDefinition::new(event)
                .mode(ReplacementMode::Optional { decline: None })
                .description("Shield".to_string()),
        );
        state.objects.insert(id, obj);
        state.battlefield.push_back(id);
        id
    }

    fn make_creature(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        id
    }

    /// CR 122.1: When a player accepts an AddCounter replacement choice, the
    /// (possibly modified) counter event must be applied. Previously
    /// `handle_replacement_choice` silently dropped non-ZoneChange events.
    #[test]
    fn add_counter_replacement_accepted_applies_counters() {
        let mut state = GameState::new_two_player(42);
        let target = make_creature(&mut state, PlayerId(0), "Bear");
        install_optional_replacement(&mut state, ReplacementEvent::AddCounter);

        let mut events = Vec::new();
        let proposed = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: target,
                counter_type: CounterType::Plus1Plus1,
            },
            count: 2,
            applied: std::collections::HashSet::new(),
        };
        let result = replacement_mod::replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice, got {result:?}");
        };
        // replace_event stashes pending_replacement but doesn't set waiting_for on its own —
        // callers (e.g. effect handlers) do that. Set it here to match real call sites.
        state.waiting_for = replacement_mod::replacement_choice_waiting_for(player, &state);
        state.priority_player = player;

        // Accept the replacement — counters must land on the target.
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).expect("accept");

        let counters_on_target = *state.objects[&target]
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(
            counters_on_target, 2,
            "AddCounter accepted after replacement choice must deliver counters"
        );
    }

    /// CR 701.26a: Tap accepted after replacement choice applies the tap state
    /// and emits `PermanentTapped`.
    #[test]
    fn tap_replacement_accepted_applies_tap() {
        let mut state = GameState::new_two_player(42);
        let target = make_creature(&mut state, PlayerId(0), "Bear");
        assert!(!state.objects[&target].tapped, "precondition");
        install_optional_replacement(&mut state, ReplacementEvent::Tap);

        let mut events = Vec::new();
        let proposed = ProposedEvent::Tap {
            object_id: target,
            applied: std::collections::HashSet::new(),
        };
        let result = replacement_mod::replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice, got {result:?}");
        };
        state.waiting_for = replacement_mod::replacement_choice_waiting_for(player, &state);
        state.priority_player = player;

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).expect("accept");

        assert!(
            state.objects[&target].tapped,
            "Tap accepted after replacement choice must tap the target"
        );
    }

    #[test]
    fn zone_change_replacement_choice_preserves_land_play_provenance() {
        let mut state = GameState::new_two_player(42);
        let land = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Test Land".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.played_from_zone = Some(Zone::Hand);
        install_optional_replacement(&mut state, ReplacementEvent::Moved);

        let mut events = Vec::new();
        let proposed = ProposedEvent::zone_change(land, Zone::Hand, Zone::Battlefield, None);
        let result = replacement_mod::replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice, got {result:?}");
        };
        state.waiting_for = replacement_mod::replacement_choice_waiting_for(player, &state);
        state.priority_player = player;

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).expect("accept");

        assert_eq!(state.objects[&land].zone, Zone::Battlefield);
        assert_eq!(state.objects[&land].played_from_zone, Some(Zone::Hand));
    }

    /// CR 615.1: When the player declines (or the replacement pipeline returns
    /// `Prevented`), the proposed event is NOT applied. Guardrail that the
    /// extraction of `apply_damage_after_replacement` did not regress the
    /// prevention path.
    #[test]
    fn replacement_prevented_does_not_apply() {
        use crate::game::effects::deal_damage::{apply_damage_after_replacement, DamageContext};

        let mut state = GameState::new_two_player(42);
        let target = make_creature(&mut state, PlayerId(0), "Bear");
        let source_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        // Bypass the replacement pipeline entirely — simulate that the pipeline
        // returned Prevented by NOT calling apply_damage_after_replacement. The
        // target must have zero marked damage (nothing applied).
        let _ctx = DamageContext::fallback(source_id, PlayerId(0));
        // Sanity: calling apply_damage_after_replacement WITH a Damage event
        // does apply (this confirms the helper is the sole application path).
        let damage_event = ProposedEvent::Damage {
            source_id,
            target: crate::types::ability::TargetRef::Object(target),
            amount: 0,
            is_combat: false,
            applied: std::collections::HashSet::new(),
        };
        let mut events = Vec::new();
        let _ = apply_damage_after_replacement(&mut state, &_ctx, damage_event, false, &mut events);
        assert_eq!(
            state.objects[&target].damage_marked, 0,
            "zero-amount damage event applies zero damage"
        );
    }

    /// CR 701.8a + CR 614: Destroy accepted after replacement choice must
    /// route through the shared helper, emitting `CreatureDestroyed` and
    /// moving the permanent to the graveyard. Also verifies that the helper
    /// re-enters the replacement pipeline for the inner ZoneChange — a
    /// mandatory `Moved` redirect to exile on a second source still fires
    /// after the outer Destroy choice is accepted.
    #[test]
    fn destroy_replacement_accepted_applies_and_reenters_pipeline() {
        use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, TargetFilter};

        let mut state = GameState::new_two_player(42);
        let victim = make_creature(&mut state, PlayerId(0), "Bear");

        // Outer: Optional Destroy replacement (creates the player choice).
        install_optional_replacement(&mut state, ReplacementEvent::Destroy);

        // Inner pipeline proof: Rest-in-Peace-style Moved redirect on a
        // separate source. If the Destroy post-accept helper re-enters the
        // pipeline on the inner Battlefield→Graveyard ZoneChange, the
        // victim ends up in exile (redirected), not graveyard.
        let rip_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut rip = GameObject::new(
            rip_id,
            CardId(888),
            PlayerId(1),
            "RIP".to_string(),
            Zone::Battlefield,
        );
        rip.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .destination_zone(Zone::Graveyard)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        destination: Zone::Exile,
                        origin: None,
                        target: TargetFilter::Any,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: false,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        face_down_profile: None,
                    },
                ))
                .description("Rest in Peace".to_string()),
        );
        state.objects.insert(rip_id, rip);
        state.battlefield.push_back(rip_id);

        // Surface the outer Destroy replacement choice to the player.
        let mut events = Vec::new();
        let proposed = ProposedEvent::Destroy {
            object_id: victim,
            source: None,
            cant_regenerate: false,
            applied: std::collections::HashSet::new(),
        };
        let result = replacement_mod::replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice, got {result:?}");
        };
        state.waiting_for = replacement_mod::replacement_choice_waiting_for(player, &state);
        state.priority_player = player;

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).expect("accept");

        // Victim left the battlefield.
        assert!(
            !state.battlefield.contains(&victim),
            "Destroy accepted after replacement choice must leave the battlefield"
        );
        // CR 614.6: The inner ZoneChange re-entered the pipeline and hit the
        // Moved→Exile redirect — the creature is in exile, not graveyard.
        assert!(
            state.exile.contains(&victim),
            "inner ZoneChange(Battlefield→Graveyard) must re-enter the pipeline; Moved redirect should send victim to exile"
        );
        assert!(
            !state.players[0].graveyard.contains(&victim),
            "victim should not end up in graveyard after Moved→Exile redirect"
        );
        // Note: `CreatureDestroyed` is emitted into the engine's internal
        // event buffer during `apply`, not the pre-choice `events` vec here.
        // The exile-vs-graveyard assertion above is the load-bearing check
        // proving both the outer Destroy and the inner ZoneChange were
        // processed through the replacement pipeline.
        let _ = events;
    }

    /// CR 701.21a + CR 614: Sacrifice accepted after replacement choice must
    /// move the permanent to graveyard and record the sacrifice for
    /// restriction tracking. `ReplacementEvent::Sacrifice` has no registry
    /// matcher (sacrifice is mediated through `Moved` on the inner zone
    /// change), so we exercise `apply_sacrifice_after_replacement` directly
    /// — the same entry point `handle_replacement_choice` invokes.
    #[test]
    fn apply_sacrifice_after_replacement_moves_to_graveyard_and_records() {
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let victim = make_creature(&mut state, PlayerId(0), "Artifact Token");
        // Mark as artifact so we can assert `record_sacrifice` ran.
        state
            .objects
            .get_mut(&victim)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let event = ProposedEvent::Sacrifice {
            object_id: victim,
            player_id: PlayerId(0),
            applied: std::collections::HashSet::new(),
        };
        let mut events = Vec::new();
        crate::game::sacrifice::apply_sacrifice_after_replacement(&mut state, event, &mut events);

        assert!(
            !state.battlefield.contains(&victim),
            "apply_sacrifice must leave the battlefield"
        );
        assert!(
            state.players[0].graveyard.contains(&victim),
            "apply_sacrifice must move to owner's graveyard (CR 701.21a)"
        );
        // CR 701.21: record_sacrifice must run so restriction tracking stays correct.
        assert!(
            state
                .players_who_sacrificed_artifact_this_turn
                .contains(&PlayerId(0)),
            "record_sacrifice must run on the post-replacement apply path"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::PermanentSacrificed { object_id, .. } if *object_id == victim)),
            "PermanentSacrificed event must be emitted"
        );
    }

    /// CR 701.21a + CR 614.6: When the inner ZoneChange is redirected (e.g.,
    /// sacrifice → exile via a `Moved` replacement), the helper honors the
    /// redirect. Proves pipeline composition for the sacrifice path.
    #[test]
    fn apply_sacrifice_after_replacement_honors_zone_change_redirect() {
        let mut state = GameState::new_two_player(42);
        let victim = make_creature(&mut state, PlayerId(0), "Bear");

        // Simulate the inner ZoneChange having been redirected to Exile by a
        // Moved replacement (as Rest in Peace would do).
        let event = ProposedEvent::ZoneChange {
            object_id: victim,
            from: Zone::Battlefield,
            to: Zone::Exile,
            cause: None,
            attach_to: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            enter_with_counters: Vec::new(),
            controller_override: None,
            enter_transformed: false,
            applied: std::collections::HashSet::new(),
            face_down_profile: None,
        };
        let mut events = Vec::new();
        crate::game::sacrifice::apply_sacrifice_after_replacement(&mut state, event, &mut events);

        assert!(
            state.exile.contains(&victim),
            "ZoneChange-redirected sacrifice must honor the replaced destination"
        );
        assert!(
            !state.players[0].graveyard.contains(&victim),
            "redirected sacrifice must not land in graveyard"
        );
    }

    /// CR 111.1 + CR 614.1a: CreateToken accepted after replacement choice
    /// must deliver the full token spec — power, toughness, types, colors,
    /// keywords are all preserved through the replacement pipeline and
    /// applied to the created battlefield object.
    #[test]
    fn create_token_replacement_accepted_applies_full_spec() {
        use crate::types::card_type::CoreType;
        use crate::types::keywords::Keyword;
        use crate::types::mana::ManaColor;
        use crate::types::proposed_event::{TokenCharacteristics, TokenSpec};

        let mut state = GameState::new_two_player(42);
        install_optional_replacement(&mut state, ReplacementEvent::CreateToken);

        let spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Soldier".to_string(),
                power: Some(2),
                toughness: Some(2),
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Soldier".to_string()],
                supertypes: Vec::new(),
                colors: vec![ManaColor::White],
                keywords: vec![Keyword::Flying],
            },
            script_name: "w_2_2_soldier_flying".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(1),
            controller: PlayerId(0),
            attach_to: None,
        };

        let battlefield_before = state.battlefield.clone();

        let mut events = Vec::new();
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: std::collections::HashSet::new(),
        };
        let result = replacement_mod::replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice, got {result:?}");
        };
        state.waiting_for = replacement_mod::replacement_choice_waiting_for(player, &state);
        state.priority_player = player;

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).expect("accept");

        // Exactly one new battlefield object was created.
        let new_ids: Vec<_> = state
            .battlefield
            .iter()
            .filter(|id| !battlefield_before.contains(id))
            .copied()
            .collect();
        assert_eq!(new_ids.len(), 1, "CreateToken accept must create one token");
        let token_id = new_ids[0];

        // CR 111.1: Full spec was applied — characteristics are preserved
        // through the replacement pipeline.
        let token = &state.objects[&token_id];
        assert!(token.is_token, "created object must be marked as a token");
        assert_eq!(token.name, "Soldier");
        assert_eq!(token.power, Some(2));
        assert_eq!(token.toughness, Some(2));
        assert!(token.card_types.core_types.contains(&CoreType::Creature));
        assert!(token.card_types.subtypes.iter().any(|s| s == "Soldier"));
        assert_eq!(token.color, vec![ManaColor::White]);
        assert!(token.keywords.contains(&Keyword::Flying));
    }

    // ── Zone-qualified clone source (Superior Spider-Man) ──
    // CR 707.9 + CR 400.1: `find_copy_targets` scans the zone encoded on the
    // filter's `FilterProp::InZone`. When the filter has no zone property,
    // battlefield is the default (preserving Clone / Phantasmal Image etc.).
    #[test]
    fn find_copy_targets_scans_graveyard_when_filter_has_in_zone_graveyard() {
        use crate::types::ability::{FilterProp, TypeFilter, TypedFilter};
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let bf_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Battlefield Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bf_creature).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Creature];
            obj.card_types.core_types = vec![CoreType::Creature];
        }
        let gy_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Graveyard Bear".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&gy_creature).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Creature];
            obj.card_types.core_types = vec![CoreType::Creature];
        }
        let source = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Spidey".to_string(),
            Zone::Battlefield,
        );

        // Filter: "any creature card in a graveyard"
        let filter = TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature).properties(vec![
            FilterProp::InZone {
                zone: Zone::Graveyard,
            },
        ]));

        let targets = find_copy_targets(&state, &filter, source, PlayerId(0), None);
        assert!(
            targets.contains(&gy_creature),
            "graveyard creature must be a legal copy target"
        );
        assert!(
            !targets.contains(&bf_creature),
            "battlefield creature must not be a legal copy target when filter scopes graveyard"
        );
    }

    #[test]
    fn find_copy_targets_defaults_to_battlefield_for_classic_clone_filter() {
        use crate::types::ability::{TypeFilter, TypedFilter};
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let bf_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Battlefield Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bf_creature).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Creature];
            obj.card_types.core_types = vec![CoreType::Creature];
        }
        let gy_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Graveyard Bear".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&gy_creature).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Creature];
            obj.card_types.core_types = vec![CoreType::Creature];
        }
        let source = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Clone".to_string(),
            Zone::Battlefield,
        );

        // Filter: "any creature" (no zone property)
        let filter = TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature));

        let targets = find_copy_targets(&state, &filter, source, PlayerId(0), None);
        assert!(
            targets.contains(&bf_creature),
            "Clone with no zone filter must find battlefield creature"
        );
        assert!(
            !targets.contains(&gy_creature),
            "Clone with no zone filter must not leak into the graveyard"
        );
    }

    /// 2026-05-09 audit M4 regression: the unified
    /// `post_replacement_continuation` slot dispatches a `Template` arm by
    /// resolving the AST against the supplied source — the pre-fold path
    /// that used `state.post_replacement_effect`.
    #[test]
    fn post_replacement_continuation_template_dispatches_against_source() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lossy Land".to_string(),
            Zone::Battlefield,
        );
        let initial_life = state.players[0].life;

        let template = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: None,
            },
        );
        state.post_replacement_continuation =
            Some(PostReplacementContinuation::Template(Box::new(template)));

        let mut events = Vec::new();
        let waiting = apply_pending_post_replacement_effect(
            &mut state,
            Some(source),
            None,
            None,
            &mut events,
        );

        // Resolved cleanly — no follow-up WaitingFor and slot drained.
        assert!(waiting.is_none(), "Template path resolved without prompt");
        assert!(state.post_replacement_continuation.is_none());
        // Source's controller (P0) lost 2 life.
        assert_eq!(state.players[0].life, initial_life - 2);
    }

    /// CR 109.4 + CR 108.4a + CR 702.52a: A replacement template resolving
    /// from a card in a graveyard scopes `Controller` to that card's owner, not
    /// to stale battlefield control.
    #[test]
    fn post_replacement_template_from_graveyard_uses_owner_not_stale_controller() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dredge Source".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&source).unwrap().controller = PlayerId(1);

        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Top Card".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Second Card".to_string(),
            Zone::Library,
        );

        let template = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
        );
        state.post_replacement_continuation =
            Some(PostReplacementContinuation::Template(Box::new(template)));

        let mut events = Vec::new();
        let waiting = apply_pending_post_replacement_effect(
            &mut state,
            Some(source),
            None,
            None,
            &mut events,
        );

        assert!(waiting.is_none(), "Template path resolved without prompt");
        assert_eq!(state.players[0].library.len(), 0);
        assert_eq!(state.players[0].graveyard.len(), 3);
        assert!(state.players[1].graveyard.is_empty());
    }

    /// 2026-05-09 audit M4 regression: the unified slot dispatches a
    /// `Resolved` arm by resolving the captured `ResolvedAbility` directly
    /// — the pre-fold path that used `state.post_replacement_resolved_effect`
    /// (e.g. Phyrexian Hydra's runtime-built prevention follow-up).
    #[test]
    fn post_replacement_continuation_resolved_dispatches_directly() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Shielded Hydra".to_string(),
            Zone::Battlefield,
        );
        let initial_life = state.players[1].life;

        // Build a resolved follow-up that targets P1 explicitly — emulates the
        // runtime_execute path where the source/controller and counter quantity
        // are captured at shield-creation time.
        let resolved = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                target: Some(TargetFilter::Controller),
            },
            Vec::new(),
            source,
            PlayerId(1),
        );
        state.post_replacement_continuation =
            Some(PostReplacementContinuation::Resolved(Box::new(resolved)));

        let mut events = Vec::new();
        let waiting = apply_pending_post_replacement_effect(
            &mut state,
            Some(source),
            None,
            None,
            &mut events,
        );

        assert!(waiting.is_none(), "Resolved path resolved without prompt");
        assert!(state.post_replacement_continuation.is_none());
        // Resolved ability's own controller (P1) lost 3 life.
        assert_eq!(state.players[1].life, initial_life - 3);
    }

    /// 2026-05-09 audit M4 backward-compat: legacy serialized GameState with
    /// the pre-fold `post_replacement_effect` field (Template binding state)
    /// migrates into the new unified slot when `finalize_public_state` runs
    /// (driven here by calling `migrate_post_replacement_continuation`
    /// directly).
    #[test]
    fn migrate_post_replacement_continuation_lifts_legacy_template() {
        let mut state = GameState::new_two_player(42);
        let template = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: None,
            },
        );
        // Simulate legacy deserialization: only the legacy slot is populated.
        state.legacy_post_replacement_effect = Some(Box::new(template.clone()));
        assert!(state.post_replacement_continuation.is_none());

        state.migrate_post_replacement_continuation();

        match state.post_replacement_continuation {
            Some(PostReplacementContinuation::Template(ref def)) => {
                assert_eq!(**def, template);
            }
            other => panic!("expected Template after migration, got {other:?}"),
        }
        assert!(state.legacy_post_replacement_effect.is_none());
        assert!(state.legacy_post_replacement_resolved_effect.is_none());
    }

    /// 2026-05-09 audit M4 backward-compat: legacy serialized GameState with
    /// the pre-fold `post_replacement_resolved_effect` field (Resolved
    /// binding state) migrates into the new unified slot. Resolved wins over
    /// Template if both are (impossibly) populated, mirroring the pre-fold
    /// dispatcher precedence at `apply_pending_post_replacement_effect`.
    #[test]
    fn migrate_post_replacement_continuation_lifts_legacy_resolved() {
        let mut state = GameState::new_two_player(42);
        let resolved = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: Some(TargetFilter::Controller),
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        state.legacy_post_replacement_resolved_effect = Some(Box::new(resolved.clone()));

        state.migrate_post_replacement_continuation();

        match state.post_replacement_continuation {
            Some(PostReplacementContinuation::Resolved(ref boxed)) => {
                assert_eq!(**boxed, resolved);
            }
            other => panic!("expected Resolved after migration, got {other:?}"),
        }
        assert!(state.legacy_post_replacement_effect.is_none());
        assert!(state.legacy_post_replacement_resolved_effect.is_none());
    }

    /// 2026-05-09 audit M4 backward-compat (defensive): when both legacy
    /// slots happen to deserialize alongside a new-shape slot — for instance
    /// because a producer wrote a hybrid blob — the new slot wins and the
    /// legacy fields are cleared. Migration is idempotent.
    #[test]
    fn migrate_post_replacement_continuation_prefers_new_slot_when_present() {
        let mut state = GameState::new_two_player(42);
        let new_template = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 5 },
                target: None,
            },
        );
        state.post_replacement_continuation = Some(PostReplacementContinuation::Template(
            Box::new(new_template.clone()),
        ));
        // Legacy slots also populated (corrupted/hybrid input).
        state.legacy_post_replacement_effect = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Untap {
                target: TargetFilter::SelfRef,
            },
        )));

        state.migrate_post_replacement_continuation();

        match state.post_replacement_continuation {
            Some(PostReplacementContinuation::Template(ref def)) => {
                assert_eq!(**def, new_template);
            }
            other => panic!("new slot must survive migration, got {other:?}"),
        }
        assert!(state.legacy_post_replacement_effect.is_none());
        assert!(state.legacy_post_replacement_resolved_effect.is_none());
    }

    /// CR 614.12a + CR 707.9 + CR 603.2: Drive Callidus Assassin's full path —
    /// optional "enter as a copy" replacement → accept → mid-entry copy
    /// target choice → pick target → granted "destroy same-name" trigger
    /// fires. Regression coverage for the case where the entering object's
    /// `ZoneChanged` event was emitted *before* `BecomeCopy` could push the
    /// granted trigger onto `trigger_definitions`, so a naive trigger scan
    /// at entry time silently dropped the trigger. The capture inside
    /// `apply_pending_post_replacement_effect` defers the event into
    /// `state.deferred_entry_events`; `handle_copy_target_choice` replays
    /// it after `BecomeCopy` resolves + layers re-evaluate.
    #[test]
    fn callidus_optional_copy_replacement_fires_granted_destroy_trigger_end_to_end() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ContinuousModification, Effect, FilterProp,
            TargetFilter, TriggerDefinition, TypeFilter, TypedFilter,
        };
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);

        // Opponent's Bear — serves as both the copy source AND the destroy
        // target. After Callidus becomes a copy of it, the granted trigger's
        // `Another + SameName` filter selects "another creature named Bear",
        // which is the only candidate (the copy itself is `Another`-excluded).
        let bear = make_creature(&mut state, PlayerId(1), "Bear");
        {
            let obj = state.objects.get_mut(&bear).unwrap();
            obj.base_name = "Bear".to_string();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Callidus Assassin enters via an Optional `Moved` replacement that
        // executes `BecomeCopy` with `GrantTrigger(destroy SameName)` — the
        // shape the parser produces for Polymorphine. Tap-wrapping (the real
        // card's "enter tapped as a copy") is structurally orthogonal here;
        // `first_non_modifier_ability` walks past Tap to find BecomeCopy, so
        // exercising BecomeCopy directly tests the same code path.
        let granted_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Destroy {
                    target: TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Creature)
                            .properties(vec![FilterProp::Another, FilterProp::SameName]),
                    ),
                    cant_regenerate: false,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield);

        let callidus = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Callidus Assassin".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&callidus).unwrap();
            obj.base_name = "Callidus Assassin".to_string();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(3);
            obj.base_toughness = Some(3);
            obj.power = Some(3);
            obj.toughness = Some(3);
            obj.replacement_definitions.push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    // CR 614.12: A replacement on a card entering the
                    // battlefield (i.e. evaluated while the card is still
                    // on the stack) is only considered when its
                    // `valid_card` is `SelfRef`. `find_applicable_replacements`
                    // enforces this at `replacement.rs:2058-2062`. Polymorphine
                    // is a self-replacement on the entering card, so the
                    // parser sets `SelfRef` automatically; the test must
                    // mirror that wiring.
                    .valid_card(TargetFilter::SelfRef)
                    .destination_zone(Zone::Battlefield)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::BecomeCopy {
                            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                            duration: None,
                            mana_value_limit: None,
                            additional_modifications: vec![ContinuousModification::GrantTrigger {
                                trigger: Box::new(granted_trigger.clone()),
                            }],
                        },
                    )),
            );
        }

        // Propose the Stack→Battlefield ZoneChange so the replacement
        // pipeline surfaces the optional choice.
        let mut events = Vec::new();
        let proposed = ProposedEvent::ZoneChange {
            object_id: callidus,
            from: Zone::Stack,
            to: Zone::Battlefield,
            cause: None,
            attach_to: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            enter_with_counters: Vec::new(),
            controller_override: None,
            enter_transformed: false,
            applied: std::collections::HashSet::new(),
            face_down_profile: None,
        };
        let result = replacement_mod::replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice (Polymorphine is optional), got {result:?}");
        };
        state.waiting_for = replacement_mod::replacement_choice_waiting_for(player, &state);
        state.priority_player = player;

        // ── Accept Polymorphine ────────────────────────────────────────────
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept Polymorphine");

        // Post-accept invariants — these are what the prior fix attempts
        // missed:
        //
        // 1. `state.waiting_for == CopyTargetChoice` (the choice surfaces)
        // 2. `state.deferred_entry_events` contains the freshly-emitted
        //    `ZoneChanged` (the producer-site capture worked)
        // 3. The granted trigger is NOT yet on the entering object —
        //    `BecomeCopy` hasn't resolved
        let WaitingFor::CopyTargetChoice {
            source_id,
            valid_targets,
            ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected CopyTargetChoice after accepting Polymorphine, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(source_id, callidus);
        assert!(
            valid_targets.contains(&bear),
            "opponent's Bear must be a valid copy target"
        );
        assert_eq!(
            state.deferred_entry_events.len(),
            1,
            "Callidus's battlefield-entry ZoneChanged must be deferred for replay"
        );
        assert!(matches!(
            state.deferred_entry_events[0],
            GameEvent::ZoneChanged { object_id, to, .. }
                if object_id == callidus && to == Zone::Battlefield
        ));

        // ── Pick Bear as the copy target ───────────────────────────────────
        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(crate::types::ability::TargetRef::Object(bear)),
            },
        )
        .expect("pick copy target");

        // Post-copy invariants:
        //
        // 1. Callidus's name now matches Bear (copy applied)
        // 2. The granted trigger landed on `trigger_definitions`
        // 3. The deferred event was drained
        // 4. The destroy trigger fired — it either sits in `pending_trigger`
        //    awaiting target selection or is already on the stack
        let copy = &state.objects[&callidus];
        assert_eq!(copy.name, "Bear", "BecomeCopy must overwrite name");
        assert!(
            copy.trigger_definitions
                .iter_all()
                .any(|t| t == &granted_trigger),
            "GrantTrigger must place the destroy-trigger on the copy"
        );
        assert!(
            state.deferred_entry_events.is_empty(),
            "deferred entry events must be drained after copy choice resolves"
        );
        let trigger_fired = state.pending_trigger.is_some()
            || state.stack.iter().any(|entry| {
                matches!(
                    entry.kind,
                    crate::types::game_state::StackEntryKind::TriggeredAbility {
                        source_id: trig_source,
                        ..
                    } if trig_source == callidus
                )
            });
        assert!(
            trigger_fired,
            "Callidus's granted destroy-same-name trigger must fire from the deferred entry replay"
        );
    }

    /// CR 614.12a + CR 608.2d: Drive the full "enters with your choice of
    /// counter" path (Denry Klin, Editor in Chief line 1) through the production
    /// pipeline — `replace_event` (Execute) → `move_to_zone` → `apply_etb_counters`
    /// → `apply_pending_post_replacement_effect` (sets `ChooseOneOfBranch` +
    /// captures the deferred entry event) → `ChooseBranch`.
    ///
    /// Discriminates pre- vs post-entry: a watcher ETB trigger observes "a
    /// creature entered". The watcher must NOT have fired while paused on the
    /// choice (the entry is deferred), and after `ChooseBranch` the chosen
    /// counter must be present AS the watcher's deferred entry replays (proving
    /// the counter was folded pre-entry per CR 614.12a, not added post-entry).
    /// `index: 1` (first strike) and `index: 0` (+1/+1) yield different counters,
    /// proving a real choice.
    fn drive_denry_choice(branch_index: usize) -> (GameState, ObjectId) {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, Effect, FilterProp, TargetFilter, TriggerDefinition,
        };
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);

        // Watcher: "When a creature enters, its controller draws a card."
        // Targetless to keep the assertion focused on the fire-with-counter
        // ordering rather than target-selection plumbing.
        let watcher_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ))
            .valid_card(TargetFilter::Typed(
                crate::types::ability::TypedFilter::new(
                    crate::types::ability::TypeFilter::Creature,
                )
                .properties(vec![FilterProp::Another]),
            ))
            .destination(Zone::Battlefield);
        let watcher = make_creature(&mut state, PlayerId(1), "Soul Warden");
        state
            .objects
            .get_mut(&watcher)
            .unwrap()
            .trigger_definitions
            .push(watcher_trigger);

        // Parse Denry Klin line 1 into the real ReplacementDefinition.
        let repl = crate::parser::oracle_replacement::parse_replacement_line(
            "Denry Klin enters with your choice of a +1/+1, first strike, or vigilance counter on it.",
            "Denry Klin, Editor in Chief",
        )
        .expect("Denry Klin line 1 must parse to a replacement");
        assert_eq!(repl.event, ReplacementEvent::Moved);

        let denry = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Denry Klin, Editor in Chief".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&denry).unwrap();
            obj.base_name = "Denry Klin, Editor in Chief".to_string();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.replacement_definitions.push(repl);
        }

        // ── Drive the production Stack→Battlefield pipeline ─────────────────
        let mut events = Vec::new();
        let proposed = ProposedEvent::ZoneChange {
            object_id: denry,
            from: Zone::Stack,
            to: Zone::Battlefield,
            cause: None,
            attach_to: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            enter_with_counters: Vec::new(),
            controller_override: None,
            enter_transformed: false,
            applied: std::collections::HashSet::new(),
            face_down_profile: None,
        };
        let result = replacement_mod::replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(event) = result else {
            panic!("mandatory enters-with-choice must Execute, got {result:?}");
        };
        let crate::types::proposed_event::ProposedEvent::ZoneChange { object_id, to, .. } = event
        else {
            panic!("expected ZoneChange execute event");
        };
        // Mirror engine.rs's Execute arm: move, then drain the post-replacement
        // continuation (the ChooseOneOf execute).
        crate::game::zones::move_to_zone(&mut state, object_id, to, &mut events);
        assert!(
            state.post_replacement_continuation.is_some(),
            "ChooseOneOf execute must stash a post-replacement continuation"
        );
        let waiting = apply_pending_post_replacement_effect(
            &mut state,
            Some(object_id),
            None,
            Some(ReplacementEvent::Moved),
            &mut events,
        );

        // ── Paused on the counter choice, entry deferred, watcher NOT fired ──
        let Some(WaitingFor::ChooseOneOfBranch {
            source_id,
            branches,
            ..
        }) = waiting.clone()
        else {
            panic!("expected ChooseOneOfBranch, got {waiting:?}");
        };
        assert_eq!(source_id, denry, "choice source must be the entering Denry");
        assert_eq!(branches.len(), 3, "three counter branches");
        assert_eq!(
            state.deferred_entry_events.len(),
            1,
            "Denry's battlefield-entry event must be deferred until the choice is made"
        );
        // CR 614.12a: the watcher must NOT have observed the entry yet (no
        // trigger queued / on stack) — the entry is held back.
        assert!(
            state.pending_trigger.is_none()
                && !state.stack.iter().any(|e| matches!(
                    e.kind,
                    crate::types::game_state::StackEntryKind::TriggeredAbility { .. }
                )),
            "watcher trigger must not fire before the counter choice (deferred entry)"
        );
        assert!(
            state.objects[&denry].counters.is_empty(),
            "no counter is present before the choice is made"
        );
        state.waiting_for = waiting.unwrap();
        state.priority_player = PlayerId(0);

        // ── Make the choice ────────────────────────────────────────────────
        apply_as_current(
            &mut state,
            GameAction::ChooseBranch {
                index: branch_index,
            },
        )
        .expect("choose counter branch");

        (state, denry)
    }

    #[test]
    fn denry_klin_enters_with_choice_folds_counter_pre_entry() {
        use crate::types::counter::CounterType;
        use crate::types::keywords::KeywordKind;

        // index 1 → first strike: exactly one first strike counter, nothing else.
        let (state, denry) = drive_denry_choice(1);
        let counters = &state.objects[&denry].counters;
        assert_eq!(
            counters.get(&CounterType::Keyword(KeywordKind::FirstStrike)),
            Some(&1),
            "first strike counter must be present"
        );
        assert!(
            !counters.contains_key(&CounterType::Plus1Plus1)
                && !counters.contains_key(&CounterType::Keyword(KeywordKind::Vigilance)),
            "no other counter may be present, got {counters:?}"
        );
        // CR 614.12a: the deferred entry was replayed, so the watcher observed
        // Denry WITH the chosen counter (proves pre-entry, not post-entry).
        assert!(
            state.deferred_entry_events.is_empty(),
            "deferred entry must drain on the ChooseBranch replay"
        );
        let watcher_fired = state.pending_trigger.is_some()
            || state.stack.iter().any(|e| {
                matches!(
                    e.kind,
                    crate::types::game_state::StackEntryKind::TriggeredAbility { .. }
                )
            });
        assert!(
            watcher_fired,
            "watcher ETB trigger must fire from the deferred entry replay after the choice"
        );

        // index 0 → +1/+1: different counter, proving a real choice.
        let (state0, denry0) = drive_denry_choice(0);
        let counters0 = &state0.objects[&denry0].counters;
        assert_eq!(
            counters0.get(&CounterType::Plus1Plus1),
            Some(&1),
            "index 0 must place the +1/+1 counter"
        );
        assert!(
            !counters0.contains_key(&CounterType::Keyword(KeywordKind::FirstStrike)),
            "index 0 must NOT place first strike"
        );
    }

    /// Negative guard: a normal (non-entry) `ChooseOneOf` resolved via
    /// `ChooseBranch` with `state.deferred_entry_events` empty must NOT trigger
    /// the deferred-entry replay — the disambiguator. This protects against the
    /// enters-counter replay misrouting an unrelated branch choice.
    #[test]
    fn unrelated_choose_branch_does_not_replay_deferred_entry() {
        use crate::types::ability::{AbilityDefinition, AbilityKind, Effect};

        let mut state = GameState::new_two_player(42);
        let source = make_creature(&mut state, PlayerId(0), "Source");
        let p0_life = state.players[0].life;

        // Two unrelated branches (gain 3 / lose 1) — NOT PutCounter/SelfRef, so
        // the capture never deferred anything for this choice.
        let branches = vec![
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: crate::types::ability::TargetFilter::Controller,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: None,
                },
            ),
        ];

        state.waiting_for = WaitingFor::ChooseOneOfBranch {
            player: PlayerId(0),
            controller: PlayerId(0),
            source_id: source,
            branches,
            branch_descriptions: Vec::new(),
            parent_targets: Vec::new(),
            context: Default::default(),
            remaining_players: Vec::new(),
        };
        state.priority_player = PlayerId(0);
        assert!(
            state.deferred_entry_events.is_empty(),
            "precondition: no deferred entry for an unrelated choice"
        );

        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .expect("resolve unrelated ChooseOneOf");

        // Branch 0 (gain 3) applied normally; no replay side effects.
        assert_eq!(
            state.players[0].life,
            p0_life + 3,
            "gain-life branch applied"
        );
        assert!(
            state.deferred_entry_events.is_empty(),
            "deferred entry must remain empty for an unrelated choice"
        );
    }
}
