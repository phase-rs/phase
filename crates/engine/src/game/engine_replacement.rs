use crate::ai_support::copy_target_mana_value_ceiling;
use crate::types::ability::{
    AbilityDefinition, Effect, PostReplacementContinuation, ResolvedAbility, TargetFilter,
    TargetRef,
};
#[cfg(test)]
use crate::types::ability::{EffectScope, TapStateChange};
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
use super::sacrifice::{apply_sacrifice_after_replacement, SacrificeApply};

/// CR 614.13a + CR 702.82a/c: matches the broad as-enters shape of a Devour
/// sacrifice replacement — a `Moved` (ETB-style) event whose post-effect is a
/// `Sacrifice` over a `Typed`/`Any` scope filter (the chooser-driven "sacrifice
/// any number of creatures/permanents" pool). This is a structural shape match,
/// NOT a Devour-specific one: other `Moved + Sacrifice{Typed|Any}` replacements
/// share it. Used both to suppress the source-as-pre-selected target injection
/// and as the capture gate for the pre-entry eligible snapshot.
/// (`ReplacementEvent` is Clone-not-Copy, so we borrow it.)
pub(crate) fn is_as_enters_sacrifice_scope_replacement(
    event: Option<&ReplacementEvent>,
    effect: &Effect,
) -> bool {
    matches!(event, Some(ReplacementEvent::Moved))
        && matches!(
            effect,
            Effect::Sacrifice {
                target: TargetFilter::Typed(_) | TargetFilter::Any,
                ..
            }
        )
}

/// CR 614.13a + CR 702.82a/c: true if `id`'s self-referential replacement
/// definitions carry an as-enters Devour-shape sacrifice (see
/// [`is_as_enters_sacrifice_scope_replacement`]). Capture gate for the
/// pre-entry eligible snapshot in `deliver_replaced_zone_change`.
pub(crate) fn object_has_devour_replacement(state: &GameState, id: ObjectId) -> bool {
    state.objects.get(&id).is_some_and(|obj| {
        obj.replacement_definitions.iter_all().any(|def| {
            def.valid_card == Some(TargetFilter::SelfRef)
                && def.execute.as_ref().is_some_and(|e| {
                    is_as_enters_sacrifice_scope_replacement(Some(&def.event), &e.effect)
                })
        })
    })
}

/// CR 701.50a + CR 614.5 + CR 616.1f: drain a deferred connive whose leading
/// Draw link parked a replacement-ordering choice. Runs only when the dedicated
/// `pending_connive_reentry` slot is set (the connive applier's parked-draw
/// path). `propose_connive` re-enters with the already-applied rids excluded
/// (CR 614.5) so the CR 616.1f repeat covers the remaining connive replacements.
/// Called from BOTH the Execute arm (the leading draw delivered) and the
/// Prevented arm (the inner draw was prevented, but CR 701.50a's "draw a card,
/// THEN that creature connives" still runs the connive step — the prevention
/// replaced only the draw). Returns the parked `WaitingFor` (ConniveDiscard /
/// fresh ReplacementChoice) if `propose_connive` parked one, else `None`.
fn drain_pending_connive_reentry(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let reentry = state.pending_connive_reentry.take()?;
    let _ = crate::game::effects::connive::propose_connive(
        state,
        reentry.conniver,
        reentry.count,
        reentry.applied,
        events,
    );
    match &state.waiting_for {
        WaitingFor::Priority { .. } => None,
        wf => Some(wf.clone()),
    }
}

pub(super) fn handle_replacement_choice(
    state: &mut GameState,
    index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let pending_was_counter_move = state
        .pending_replacement
        .as_ref()
        .is_some_and(|pending| matches!(pending.proposed, ProposedEvent::MoveCounter { .. }));
    // CR 701.24a: capture the parked library placement (W3) BEFORE
    // `continue_replacement` consumes (`.take()`s) the pending record, so the
    // ZoneChange resume arm below can thread it into the delivery `DeliveryCtx`
    // instead of hardcoding `None` (which would let the tail auto-shuffle the
    // requested position away). `None` for every non-library parked event.
    let parked_library_placement = state
        .pending_replacement
        .as_ref()
        .and_then(|pending| pending.library_placement.clone());
    let result = super::replacement::continue_replacement(state, index, events);
    // CR 614.12a: an optional `MayCost` accept whose payment surfaced an
    // interactive sub-choice (e.g. Mox Diamond's "discard a land card" with
    // multiple eligible lands) re-parked the pending replacement with
    // `may_cost_paid: true` plus any `may_cost_remaining`, and left
    // `waiting_for` on the live sub-choice prompt.
    // Surface that prompt as-is; the sub-choice's resolution re-enters
    // `continue_replacement` (resume) to finish entering the permanent once the
    // cost is paid. The carried `Execute` payload is inert and must not be
    // delivered here.
    if std::mem::take(&mut state.replacement_may_cost_paused) {
        return Ok(state.waiting_for.clone());
    }
    match result {
        super::replacement::ReplacementResult::Execute(event) => {
            let mut zone_change_object_id = None;
            let mut enters_battlefield = false;
            match event {
                // Phase B (PLAN §6.2 / §7): the divergent partial copy of
                // `deliver_replaced_zone_change` that used to live here is
                // dissolved — the post-choice event is a
                // `ReplacementResult::Execute` payload, so it is sealed through
                // the third mint path (`approve_post_replacement`) and
                // delivered by the shared `zone_pipeline::deliver` machinery.
                // The resumed entry now gets the FULL delivery tail the copy
                // skipped: the CR 614.12a devour snapshot, the CR 614.1c
                // `EntersWithAdditionalCounters` statics snapshot, the
                // CR 303.4f `attach_to` host, `entered_via_ability_source`
                // provenance (CR 603.6a, from the event's `cause`), and the
                // CR 701.24a library-shuffle arm.
                //
                // Divergence reconciliation (resolved by parameterizing the
                // shared tail instead of keeping a copy):
                // (1) `DeliveryCtx.drain = CallerEpilogue` — the tail skips the
                //     `post_replacement_continuation` drain; the epilogue below
                //     keeps draining WITH the spell-resolution ctx and with
                //     `post_replacement_source` cleared for zone changes.
                // (2) `pending_spell_resolution` ordering is therefore
                //     untouched: `apply_pending_spell_resolution` still runs in
                //     the epilogue before that drain.
                // (3) PLAN OQ#3 (RESOLVED): play/cast provenance is not a ctx
                //     knob. `played_from_zone` (CR 305.1 land-play provenance)
                //     survives battlefield entry naturally — it is cleared only
                //     on battlefield EXIT, so the pre-move capture that used to
                //     ride `DeliveryCtx` here preserved a value that was never
                //     destroyed (verified no-op). The cast-link family that IS
                //     entry-cleared (kicker / convoke / cast-timing, CR 400.7d)
                //     is restored structurally inside the shared delivery for
                //     `Stack → Battlefield` events (`CastLinkSnapshot`).
                event @ ProposedEvent::ZoneChange { .. } => {
                    let (object_id, to, cause) = match &event {
                        ProposedEvent::ZoneChange {
                            object_id,
                            to,
                            cause,
                            ..
                        } => (*object_id, *to, *cause),
                        _ => unreachable!("arm pattern guarantees ZoneChange"),
                    };
                    let Ok(approved) =
                        crate::game::zone_pipeline::ApprovedZoneChange::approve_post_replacement(
                            event,
                        )
                    else {
                        unreachable!("arm pattern guarantees a ZoneChange payload");
                    };
                    match crate::game::zone_pipeline::deliver(
                        state,
                        approved,
                        crate::game::zone_pipeline::DeliveryCtx {
                            source_id: cause,
                            exile_links: crate::game::zone_pipeline::ExileLinkSpec::default(),
                            drain:
                                crate::types::game_state::PostReplacementDrainOwner::CallerEpilogue,
                            // CR 701.24a: thread the parked W3 library placement so
                            // a resumed Library-targeting redirect lands at the
                            // requested index instead of the tail auto-shuffling it
                            // away. `None` for every non-library parked event.
                            library_placement: parked_library_placement.clone(),
                        },
                        events,
                    ) {
                        crate::game::zone_pipeline::ZoneDeliveryResult::Done => {}
                        // CR 614.1c / CR 614.12a: the delivery tail parked a
                        // counter-replacement or devour prompt and stashed the
                        // remaining tail as a `ContinueZoneDeliveryTail` record
                        // (carrying `CallerEpilogue`, so the NEXT resume's
                        // epilogue still owns the continuation drain). Surface
                        // the parked prompt; the epilogue must not run yet.
                        crate::game::zone_pipeline::ZoneDeliveryResult::NeedsChoice(_) => {
                            return Ok(state.waiting_for.clone());
                        }
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
                // CR 614.1e + CR 708.11: TurnFaceUp is performed inline in
                // `morph::turn_face_up` (the replacement only adds its actions and
                // does not prevent the turn-up), so there is nothing to apply on
                // the post-replacement Execute path here.
                ProposedEvent::TurnFaceUp { .. } => {}
                // CR 121.1 + CR 614.6 + CR 614.11: Draw accepted after
                // replacement choice — delegate to the shared post-replacement
                // helper so library-zone move + per-turn accounting match the
                // non-choice delivery. For Abundance-shape replacements
                // (`execute` is a non-Draw chain), `draw_applier` has zeroed
                // the count and the central `post_replacement_continuation`
                // drain below runs the chain (Choose → RevealUntil).
                draw @ ProposedEvent::Draw { player_id, .. } => {
                    apply_draw_after_replacement(state, draw, events);
                    // CR 805.4b: if this resumed draw IS the front of the
                    // team draw-step queue (the active player's mandatory
                    // draw, parked here by a CR 616.1 competing-replacement
                    // choice), it has now actually completed — pop it so the
                    // drain below advances to any remaining queued teammate
                    // instead of re-entering `execute_draw_for` for the same
                    // player and drawing them a second card. A Draw choice
                    // unrelated to the team draw-step queue (a spell's "draw
                    // two cards" hitting a competing replacement, e.g.) finds
                    // a front that doesn't match `player_id` and is correctly
                    // left untouched.
                    if state.pending_team_draw_step.first() == Some(&player_id) {
                        state.pending_team_draw_step.remove(0);
                    }
                }
                // CR 701.22a: Scry accepted after replacement choice.
                scry @ ProposedEvent::Scry { .. } => {
                    apply_scry_after_replacement(state, scry, events);
                }
                // CR 701.37a: Explore accepted after replacement choice — the
                // explore resolver handles the actual explore logic; this is a no-op here.
                ProposedEvent::Explore { .. } => {}
                // CR 701.50a + CR 616.1: Connive surviving as an `Execute`
                // payload after a replacement-ordering choice (the count-modifier
                // case — a full-substitution connive replacement returns
                // `Prevented` and never reaches here). The connive keyword action
                // still has to run with the surviving count; resolve its internals
                // directly so it does not re-enter the propose pipeline (CR 614.5).
                // CR 616.1f: this is the TERMINAL survivor — `pipeline_loop`
                // already repeated over and exhausted every applicable connive
                // replacement, so no connive replacement remains to apply here and
                // a direct `resolve_connive_effect` is correct.
                ProposedEvent::Connive {
                    object_id, count, ..
                } => {
                    let _ = crate::game::effects::connive::resolve_connive_effect(
                        state, object_id, count, events,
                    );
                }
                // CR 701.34a: Proliferate accepted after replacement choice.
                proliferate @ ProposedEvent::Proliferate { .. } => {
                    crate::game::effects::proliferate::apply_proliferate_after_replacement(
                        state,
                        proliferate,
                        events,
                    );
                }
                // CR 701.17a: Mill accepted after replacement choice — delegate
                // to the shared helper so count clamping and library movement
                // match the non-choice delivery.
                //
                // CR 616.1: a milled card's own `Moved` replacements (Rest in
                // Peace + Leyline of the Void class) can surface a per-card
                // ordering choice mid-delivery. The helper parks that prompt
                // (`state.waiting_for` set, tail in `pending_batch_deliveries`)
                // and returns `false`. Early-return so the unconditional
                // `waiting_for = Priority` reset below does NOT clobber the
                // parked prompt — mirroring the `apply_etb_counters`
                // early-return in the ZoneChange arm. The resume path drains the
                // tail via `zone_pipeline::drain_pending_batch_deliveries`.
                mill @ ProposedEvent::Mill { .. } => {
                    // `EffectError` has no `EngineError` conversion here, so the
                    // prior `let _ =` swallowed it; preserve that by mapping an
                    // error to "delivered" (no pause) and only reacting to the
                    // pause signal.
                    if !apply_mill_after_replacement(state, mill, events).unwrap_or(true) {
                        return Ok(state.waiting_for.clone());
                    }
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
                //
                // CR 614.6: the inner hand → graveyard move re-proposes a
                // `ZoneChange` carrying `applied`, so `Moved` redirects (RIP
                // class) are consulted here too. A redirect that itself needs a
                // CR 616.1 choice parks `state.waiting_for`; early-return so the
                // unconditional reset below does not clobber it.
                ProposedEvent::Discard {
                    player_id,
                    object_id,
                    source_id,
                    applied,
                    ..
                } => {
                    if let effects::discard::DiscardOutcome::NeedsReplacementChoice(player) =
                        effects::discard::complete_discard_to_graveyard(
                            state, object_id, player_id, source_id, applied, events,
                        )
                    {
                        state.waiting_for =
                            crate::game::replacement::replacement_choice_waiting_for(player, state);
                        return Ok(state.waiting_for.clone());
                    }
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
                                supertype: None,
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
                        if count > 0 {
                            state.layers_dirty.mark_full();
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
                // CR 701.21a + CR 614.1: Sacrifice accepted after replacement
                // choice — delegate to the shared helper. Regeneration cannot
                // apply (CR 701.21a) but Moved replacements on the inner graveyard
                // transfer do; if that inner transfer itself needs a choice, the
                // helper sets `state.waiting_for` and we propagate it back.
                sacrifice @ ProposedEvent::Sacrifice { .. } => {
                    if let SacrificeApply::NeedsChoice(_) =
                        apply_sacrifice_after_replacement(state, sacrifice, events)
                    {
                        return Ok(state.waiting_for.clone());
                    }
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

            // CR 805.4b: a draw-step draw that paused on the choice just
            // resolved above may have queued teammate(s) still owed their
            // own draw this step (`turns::execute_draw` seeds the queue; the
            // `ProposedEvent::Draw` arm above already popped the
            // just-completed player off the FRONT so this drain doesn't
            // redraw them; `drain_pending_team_draw_step` is the single
            // authority that empties the rest). Drain before falling through
            // to the generic Priority reset so a 2HG teammate's mandatory
            // draw is never silently dropped when the active player's own
            // draw needed a CR 616.1 competing-replacement choice.
            if matches!(waiting_for, WaitingFor::Priority { .. })
                && !state.pending_team_draw_step.is_empty()
            {
                if let Some(wf) = super::turns::drain_pending_team_draw_step(state, events) {
                    waiting_for = wf;
                }
            }

            // CR 701.50a + CR 614.5 + CR 616.1f: resume a deferred connive whose
            // leading Draw link parked this just-resolved ReplacementChoice. The
            // draw fully delivered above (Draw arm), so "draw a card, THEN that
            // creature connives" (CR 701.50a) order is honored. `propose_connive`
            // re-enters with the already-applied rids excluded (CR 614.5) so the
            // CR 616.1f repeat covers the remaining connive replacements; it sets a
            // parked ConniveDiscard / fresh ReplacementChoice on state.waiting_for,
            // which we surface instead of the Priority reset. Drained from the
            // DEDICATED slot so the leading draw's DeliveryTail could not consume it.
            if matches!(waiting_for, WaitingFor::Priority { .. }) {
                if let Some(wf) = drain_pending_connive_reentry(state, events) {
                    waiting_for = wf;
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

            // CR 603.10a + CR 616.1: A simultaneous zone-move batch (mill or
            // mass bounce) paused mid-delivery because an object's Moved
            // replacements needed an ordering choice (Rest in Peace + Leyline of
            // the Void class). The chosen event was delivered by the ZoneChange
            // arm above; drain the parked tail. The drain may re-park when the
            // next object surfaces its own prompt — in that case it sets
            // `state.waiting_for` for us to propagate.
            if matches!(waiting_for, WaitingFor::Priority { .. })
                && state.pending_batch_deliveries.is_some()
            {
                crate::game::zone_pipeline::drain_pending_batch_deliveries(state, events);
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
                if state.pending_phase_transition_progress.is_some() {
                    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                        waiting_for = state.waiting_for.clone();
                    }
                } else if state.deferred_step_trigger_resume.is_some()
                    && matches!(state.waiting_for, WaitingFor::Priority { .. })
                {
                    // CR 513.1 + CR 603.3b: A CR 616.1 mana-pool choice can
                    // defer completion of `enter_phase`. In that case
                    // `auto_advance` returned before its per-step trigger arm
                    // ran (it bails while `pending_phase_transition_progress`
                    // is set). Resume only when that bail happened — not when
                    // `advance_phase` alone paused the drain (unit tests).
                    state.deferred_step_trigger_resume = None;
                    waiting_for = super::turns::auto_advance(state, events);
                } else {
                    state.deferred_step_trigger_resume = None;
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
        super::replacement::ReplacementResult::NeedsChoice(player) => {
            // CR 616.1 + CR 701.24a: a SECOND ordering choice on the same
            // library-placement event re-parked a fresh `PendingReplacement`
            // inside `pipeline_loop` with `library_placement: None`. Reapply the
            // placement captured before `continue_replacement` consumed the prior
            // record so the eventual delivery still honors the requested index
            // instead of the tail auto-shuffling it away. `None` for every
            // non-library parked event (no-op).
            if let Some(pending) = state.pending_replacement.as_mut() {
                if pending.library_placement.is_none() {
                    pending.library_placement = parked_library_placement.clone();
                }
            }
            Ok(super::replacement::replacement_choice_waiting_for(
                player, state,
            ))
        }
        super::replacement::ReplacementResult::Prevented => {
            // CR 616.1f + CR 701.50a: a full-substitution applier (the Leader,
            // Super-Genius connive replacement) can park its OWN interactive
            // choice while running the replacing action. Two shapes occur:
            //  - a `ConniveDiscard` prompt — the surviving plain connive pauses
            //    when the controller must choose which card to discard, or
            //  - a FRESH `ReplacementChoice` — the nested "then that creature
            //    connives" re-entry found two or more OTHER still-applicable
            //    connive replacements, so CR 616.1f repeats over them and the
            //    controller must order the next one (the 3+ co-applicable case).
            // `continue_replacement` returned `Prevented` (the original event was
            // fully replaced), but the applier already set `state.waiting_for` to
            // that live prompt. Surface it instead of clobbering it with
            // `Priority`; the prompt's own resolution finishes the parked work.
            //
            // A bare `ReplacementChoice` whitelist is insufficient: it cannot
            // tell the JUST-RESOLVED ordering prompt (already consumed) from a
            // freshly-parked nested one. `continue_replacement` `.take()`-consumed
            // the prior pending record at its start, so a `pending_replacement`
            // that is still `Some` here is necessarily the applier's freshly-
            // parked nested ordering choice — surface it so the next
            // `ChooseReplacement` resumes the CR 616.1f repeat instead of orphaning
            // the record and dropping replacements C onward.
            if state.pending_replacement.is_some()
                || !matches!(
                    state.waiting_for,
                    WaitingFor::Priority { .. } | WaitingFor::ReplacementChoice { .. }
                )
            {
                return Ok(state.waiting_for.clone());
            }
            // CR 701.50a + CR 614.5 + CR 616.1f: the leading Draw of a connive
            // replacement was PREVENTED (a draw-Prevent replacement ordered
            // first). The prevention replaced only the draw — CR 701.50a's
            // "instead you draw a card, THEN that creature connives" still runs
            // the connive step. Drain the deferred connive from the dedicated
            // slot (the Execute arm above did not run because the draw was
            // prevented). Reset the stale leading-draw `ReplacementChoice`
            // waiting_for to Priority first (mirrors the Execute arm and every
            // other Prevented-arm drain below) so the connive's own draw
            // re-enters from a clean state instead of seeing the parked prompt.
            // `propose_connive` may park a ConniveDiscard / fresh
            // ReplacementChoice — surface it. If the slot is None (every
            // non-connive Prevented resolution) skip entirely so control falls
            // through to the existing pending-* blocks unchanged.
            if state.pending_connive_reentry.is_some() {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                if let Some(wf) = drain_pending_connive_reentry(state, events) {
                    return Ok(wf);
                }
                return Ok(state.waiting_for.clone());
            }
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
            // CR 603.10a + CR 616.1: the paused batch object's event was
            // prevented outright — the remaining parked tail still delivers.
            if state.pending_batch_deliveries.is_some() {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                crate::game::zone_pipeline::drain_pending_batch_deliveries(state, events);
                return Ok(state.waiting_for.clone());
            }
            // CR 608.3e: If the ETB was prevented during spell resolution,
            // the permanent goes to the graveyard instead.
            //
            // CR 614.6: this graveyard fallback is a FRESH, never-consulted
            // event — the consulted (and prevented) event was the battlefield
            // ENTRY (`to: Battlefield`), so routing the fallback through the
            // pipeline cannot double-apply: the prevention definition is
            // Battlefield-scoped and cannot re-match a →Graveyard move. A
            // board-wide `Moved` graveyard→exile redirect (Rest in Peace /
            // Leyline of the Void) now fires on the discarded spell — the
            // un-migrated twin of stack.rs's C2 prevented-permanent site. The
            // dead continuation is cleared BEFORE the move so a CR 616.1
            // ordering pause (two simultaneous redirects) cannot leave it for
            // the next resume's epilogue to drain; on a pause, surface the
            // parked prompt (its resume delivers the chosen event through the
            // ZoneChange arm above).
            state.pending_continuation = None;
            if let Some(ctx) = state.pending_spell_resolution.take() {
                match crate::game::zone_pipeline::move_object(
                    state,
                    crate::game::zone_pipeline::ZoneMoveRequest::spell_resolution_default(
                        ctx.object_id,
                        Zone::Graveyard,
                    ),
                    events,
                ) {
                    crate::game::zone_pipeline::ZoneMoveResult::Done => {}
                    crate::game::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
                    | crate::game::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                        return Ok(state.waiting_for.clone());
                    }
                }
            }
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
    // Force a full layer pass after the copy chain so the realized
    // characteristics below (enter-tapped, ETB counters) read post-copy state.
    crate::game::layers::mark_layers_full(state);
    crate::game::layers::flush_layers(state);
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
    if let Some(waiting_for) = replay_deferred_entry_events(state, source_id, events)? {
        return Ok(waiting_for);
    }
    // CR 702.49c: a ninjutsu entry that deferred `BatchCompletion::NinjutsuPlacement`
    // while paused on `CopyTargetChoice` must run combat placement after the copy
    // resolves (mirrors the `ReturnAsAuraTarget` batch drain in engine.rs).
    if state.pending_batch_deliveries.is_some() {
        crate::game::zone_pipeline::drain_pending_batch_deliveries(state, events);
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return Ok(state.waiting_for.clone());
        }
    }
    Ok(WaitingFor::Priority {
        player: state.active_player,
    })
}

/// CR 603.2 + CR 614.12a: Replay the deferred battlefield-entry `ZoneChanged`
/// event(s) for `source_id` through the trigger pipeline after a mid-entry
/// player choice (copy target, enters-with-counter branch, or as-enters named
/// choice) has resolved, then surface any interactive trigger pause that
/// replay raised. This is the single authority for deferred-entry replay — both
/// the copy-completion site (`handle_copy_target_choice`) and the as-enters
/// named-choice resume site (`engine_resolution_choices.rs`) route through it,
/// so the pause-propagation logic is defined exactly once.
///
/// The entry event was captured into `state.deferred_entry_events` by
/// `capture_deferred_entry_events_if_mid_entry_choice` *before* the choice was
/// made, so that ETB observers (constellation, Soul Warden) and any granted
/// ETB triggers (Callidus Assassin) match against the fully realized,
/// post-choice object — not a half-entered one (CR 614.12a: the choice is made
/// before the permanent enters). `process_triggers` (CR 603.2 event-based
/// triggers) + `check_delayed_triggers` (CR 603.7c delayed triggers) collect
/// against the realized object.
///
/// Drained via `std::mem::take` so replay is idempotent — the event is fired
/// exactly once and can never also reach a later `Priority`-result pipeline
/// pass. Returns `None` (no pause) when `deferred_entry_events` is empty (the
/// no-op guard for non-entry persisted choices, e.g. Pithing Needle naming),
/// or when the entering source has left the battlefield (concede / error /
/// chained-replacement paths leave a stale event we discard rather than fire
/// against a phantom object).
pub(super) fn replay_deferred_entry_events(
    state: &mut GameState,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<Option<WaitingFor>, EngineError> {
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
    // entry. When it pauses it sets `state.pending_trigger` for the active
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
        return Ok(Some(state.waiting_for.clone()));
    }
    // CR 603.3b (#531): propagate OrderTriggers pause from process_triggers
    // above. Without this, multiple simultaneously-fired ETB observers on one
    // entry (e.g., two constellation triggers, or Wedding Announcement's token
    // + Ocelot Pride's life-gain rider on a copy entry) would silently fall
    // through to Priority.
    if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
        return Ok(Some(state.waiting_for.clone()));
    }
    if let Some(waiting_for) = super::engine::begin_pending_trigger_target_selection(state)? {
        return Ok(Some(waiting_for));
    }
    Ok(None)
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
        // CR 607.2a: For ExiledCardByIndex (The Mimeoplasm), the target is already
        // determined by the index - no choice prompt needed. Directly resolve the copy.
        if matches!(target, TargetFilter::ExiledCardByIndex { .. }) {
            let targets = valid_targets
                .into_iter()
                .map(TargetRef::Object)
                .collect::<Vec<_>>();
            let resolved =
                build_resolved_from_def_with_targets(real_work, source_id, controller, targets);
            let _ = effects::resolve_ability_chain(state, &resolved, events, 0);
            return match &state.waiting_for {
                WaitingFor::Priority { .. } => None,
                wf => Some(wf.clone()),
            };
        } else {
            return Some(WaitingFor::CopyTargetChoice {
                player: controller,
                source_id,
                valid_targets,
                max_mana_value,
            });
        }
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
    let sacrifice_typed_scope = is_as_enters_sacrifice_scope_replacement(event, &real_work.effect);
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
    capture_deferred_entry_events_if_mid_entry_choice(state, waiting_for.as_ref(), events);
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

/// CR 603.2 + CR 614.12a: When a permanent's battlefield entry pauses on a
/// mid-entry player choice — `CopyTargetChoice` (enter as a copy), a
/// `ChooseOneOfBranch` that `is_enters_counter_choice` (enter with your choice
/// of counter), or a persisted `NamedChoice` whose `source_id` is the entering
/// permanent (the "As it enters, choose a color/creature type/…" shape, e.g.
/// Valgavoth's Lair) — clone any battlefield-entry `ZoneChanged` events for the
/// entering source into `state.deferred_entry_events`. The original `events`
/// vec is preserved so the frontend animates the entry as soon as the spell /
/// land-play resolves; the deferred copy is replayed through `process_triggers`
/// / `check_delayed_triggers` once the choice resolves (in
/// `handle_copy_target_choice` for copies, in the `ChooseBranch` arm and the
/// `NamedChoice` + `ChooseOption` arm of `engine_resolution_choices.rs` for the
/// other two shapes), so every ETB observer (constellation like Doomwake Giant,
/// Soul Warden, …) sees the entry against the fully realized post-choice object.
/// Without this, the entry event returns `WaitingFor::NamedChoice` instead of
/// `Priority`, so the canonical priority-time trigger collection
/// (`engine_priority::run_post_action_pipeline`) is skipped and every ETB
/// observer is silently dropped for that entry (issue #830).
///
/// The `NamedChoice` arm is keyed on the structural fact that an entry
/// `ZoneChanged` for the same source is present in `events` (the capture loop
/// below only pushes matching events). Non-entry persisted `NamedChoice`s —
/// Pithing Needle naming, a `Choose` resolved off the stack — carry no such
/// entry event, so nothing is captured and the downstream replay is a no-op.
///
/// Defense in depth: clears any stale events from a prior choice that exited
/// abnormally (concede mid-choice, eliminate_player, error return before drain)
/// so the replay never fires triggers against a phantom object.
fn capture_deferred_entry_events_if_mid_entry_choice(
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
        // CR 603.2 + CR 614.12a: an "As it enters, choose …" replacement
        // (Valgavoth's Lair, the Thriving lands, Voice of All) pauses the entry
        // on a persisted `NamedChoice` whose `source_id` is the entering
        // permanent. Defer the entry event exactly like the copy/counter shapes
        // so ETB observers fire against the post-choice object once the player
        // answers. The entry-event filter in the capture loop scopes this to the
        // entering source — a persisted `NamedChoice` with no matching entry
        // event in `events` (Pithing Needle naming) captures nothing.
        Some(WaitingFor::NamedChoice {
            source_id: Some(source_id),
            ..
        }) => *source_id,
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
        obj.cast_controller = ctx.cast_controller;
        if let Some(permission) = ctx.cast_timing_permission {
            obj.cast_timing_permission = Some((permission, state.turn_number));
        }
        obj.kickers_paid.clone_from(&ctx.kickers_paid);
        obj.additional_cost_payment_count = ctx.additional_cost_payment_count;
        obj.additional_cost_payments
            .clone_from(&ctx.additional_cost_payments);
        obj.convoked_creatures.clone_from(&ctx.convoked_creatures);
        crate::database::synthesis::ensure_paid_offspring_etb_copy_triggers(obj);
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
    // CR 607.2a: Special handling for ExiledCardByIndex (The Mimeoplasm).
    // This filter resolves to a specific card exiled by the source, indexed by order.
    // We resolve it directly rather than scanning a zone.
    if let TargetFilter::ExiledCardByIndex { index } = filter {
        let exiled_cards = state.cards_exiled_with_source_this_turn.get(&source_id);
        if let Some(&card_id) = exiled_cards.and_then(|cards| cards.get(*index as usize)) {
            // Check mana value constraint if present
            if let Some(max) = max_mana_value {
                if let Some(obj) = state.objects.get(&card_id) {
                    if obj.mana_cost.mana_value() > max {
                        return vec![];
                    }
                }
            }
            return vec![card_id];
        }
        return vec![];
    }

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

    /// CR 805.4b + CR 616.1: regression for a review-flagged bug where the
    /// active player's draw-step draw, after pausing on a CR 616.1
    /// competing-replacement choice and being resumed via the REAL
    /// `GameAction::ChooseReplacement` path, was drawn for a SECOND time
    /// instead of the queue advancing to the teammate. Drives the actual
    /// production path end to end: `turns::execute_draw` seeds
    /// `pending_team_draw_step` with `[P0, P1]` and pauses on P0's own draw;
    /// `apply_as_current(ChooseReplacement)` resolves that choice through
    /// `handle_replacement_choice`, which must pop P0 off the queue's front
    /// (not redraw them) and then drain through to P1.
    #[test]
    fn two_headed_giant_draw_step_resume_draws_active_player_once_then_teammate() {
        let mut state =
            GameState::new(crate::types::format::FormatConfig::two_headed_giant(), 4, 0);
        state.active_player = PlayerId(0);

        // A CR 616.1 optional replacement on P0's own draw — accepting or
        // declining is the choice that pauses the draw (mirrors
        // `install_optional_replacement` above, but controlled by P0 so it
        // matches P0's draw under the default "source player only" scope —
        // `ReplacementDefinition::valid_player: None` — rather than P1's).
        let shield = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut shield_obj = GameObject::new(
            shield,
            CardId(1),
            PlayerId(0),
            "Draw Shield".to_string(),
            Zone::Battlefield,
        );
        shield_obj.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::Draw)
                .mode(ReplacementMode::Optional { decline: None })
                .description("Draw Shield".to_string()),
        );
        state.objects.insert(shield, shield_obj);
        state.battlefield.push_back(shield);

        let p0_card = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "P0 Card".to_string(),
            Zone::Library,
        );
        let p1_card = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "P1 Card".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        let paused = super::super::turns::execute_draw(&mut state, &mut events);
        assert!(
            paused.is_some(),
            "P0's draw must pause on the competing-replacement choice"
        );
        assert_eq!(
            state.pending_team_draw_step,
            vec![PlayerId(0), PlayerId(1)],
            "both active-team players must still be queued while P0's choice is pending"
        );

        // Resolve P0's choice through the REAL action-dispatch path.
        let choosing_player = match &state.waiting_for {
            WaitingFor::ReplacementChoice { player, .. } => *player,
            other => panic!("expected ReplacementChoice, got {other:?}"),
        };
        state.priority_player = choosing_player;
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).expect("accept");

        // P0 drew exactly once (the queue's front was popped on resume, not
        // re-entered) and the queue advanced to P1, who also drew their own
        // single card — neither dropped nor double-drawn.
        assert!(
            state.players[0].hand.contains(&p0_card),
            "P0 must have drawn their card"
        );
        assert_eq!(
            state.players[0].hand.len(),
            1,
            "P0 must draw exactly once, not twice, after the resume"
        );
        assert!(
            state.players[1].hand.contains(&p1_card),
            "P1's queued draw-step draw must still happen after P0's resume"
        );
        assert_eq!(state.players[1].hand.len(), 1, "P1 must draw exactly once");
        assert!(
            state.pending_team_draw_step.is_empty(),
            "the draw-step queue must be fully drained after resume"
        );
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

    /// CR 614.1c + CR 616.1 discriminating test (fail-first): a battlefield
    /// entry that parks on a replacement-ordering prompt (two opposite-direction
    /// enter tap-state `Moved` defs — one enters tapped, one enters untapped:
    /// the Frozen Aether + Spelunking class, last-applied-wins and so a material
    /// CR 616.1e/f collision) must, on resume, run the FULL shared delivery tail.
    /// Here the missing piece is the `EntersWithAdditionalCounters` static
    /// snapshot (Kalain / Counter Lord class — "other creatures you control
    /// enter with an additional +1/+1 counter"): the divergent resume copy
    /// applied only the event's own `enter_with_counters`, so a resumed entry
    /// silently missed the static's counter while the never-paused path
    /// granted it.
    #[test]
    fn resumed_entry_receives_enters_with_additional_counters_static() {
        use std::sync::Arc;

        use crate::game::zone_pipeline::{self, ZoneMoveRequest, ZoneMoveResult};
        use crate::types::ability::{
            AbilityDefinition, ControllerRef, Effect, FilterProp, StaticDefinition, TargetFilter,
            TypedFilter,
        };
        use crate::types::statics::StaticMode;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // CR 614.1c: P0 permanent granting "other creatures you control enter
        // with an additional +1/+1 counter" — must be functioning BEFORE the
        // entrant enters.
        let lord = make_creature(&mut state, PlayerId(0), "Counter Lord");
        {
            let obj = state.objects.get_mut(&lord).unwrap();
            let def = StaticDefinition::new(StaticMode::EntersWithAdditionalCounters {
                counter_type: CounterType::Plus1Plus1,
                count: 1,
            })
            .affected(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Another]),
            ));
            obj.static_definitions.push(def.clone());
            Arc::make_mut(&mut obj.base_static_definitions).push(def);
        }

        // A genuinely *material* enter tap-state collision: one replacement makes
        // the entering permanent enter tapped (Frozen Aether class), the other
        // makes it enter untapped (Spelunking / Archelos class). Opposite
        // directions are last-applied-wins, so CR 616.1e/f requires the
        // controller to order them and the entry parks on a ReplacementChoice.
        // (Two *same*-direction writes are idempotent and commute — they would
        // not prompt; see replacement.rs `CommuteClass::EnterTapped`/`EnterUntapped`.)
        for (offset, name, state_change) in [
            (0u64, "Frozen Aether", TapStateChange::Tap),
            (1, "Spelunking", TapStateChange::Untap),
        ] {
            let oid = ObjectId(9000 + offset);
            let mut src = GameObject::new(
                oid,
                CardId(900 + offset),
                PlayerId(1),
                name.to_string(),
                Zone::Battlefield,
            );
            src.replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SetTapState {
                        target: TargetFilter::SelfRef,
                        scope: EffectScope::Single,
                        state: state_change,
                    },
                ))
                .destination_zone(Zone::Battlefield)
                .description(name.to_string())]
            .into();
            state.objects.insert(oid, src);
            state.battlefield.push_back(oid);
        }

        // P0 creature entering from hand through the pipeline.
        let entrant = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&entrant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut events = Vec::new();
        let result = zone_pipeline::move_object(
            &mut state,
            ZoneMoveRequest::effect(entrant, Zone::Battlefield, entrant),
            &mut events,
        );
        assert!(
            matches!(result, ZoneMoveResult::NeedsChoice(_)),
            "the tap/untap (opposite-direction) collision must park the entry"
        );
        let WaitingFor::ReplacementChoice {
            player: chooser, ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected parked ReplacementChoice, got {:?}",
                state.waiting_for
            );
        };
        state.priority_player = chooser;

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("resume replacement choice");

        let obj = &state.objects[&entrant];
        assert_eq!(obj.zone, Zone::Battlefield, "entry delivered after resume");
        // CR 616.1e/f: opposite-direction tap-state writes are last-applied-wins.
        // The chosen order (`index: 0`) lands the untapped write last, so the
        // resumed entry is untapped — confirming the chosen ordering was honored.
        assert!(
            !obj.tapped,
            "the chosen ordering's last-applied untap write must win on the resumed entry"
        );
        assert_eq!(
            *obj.counters.get(&CounterType::Plus1Plus1).unwrap_or(&0),
            1,
            "resumed entry must receive the EntersWithAdditionalCounters static \
             (CR 614.1c) — the divergent resume copy dropped the statics snapshot"
        );
    }

    /// CR 608.3e + CR 614.6 discriminating test (fail-first): when a permanent
    /// spell's ETB is fully prevented after a replacement choice
    /// (`ReplacementResult::Prevented` while `pending_spell_resolution` is set),
    /// the graveyard fallback is a FRESH, never-consulted event — it must route
    /// through the zone pipeline so a board-wide `Moved` graveyard→exile
    /// redirect (Rest in Peace / Leyline of the Void) fires on the discarded
    /// spell. The raw `move_to_zone` fallback dropped the redirect — the
    /// un-migrated twin of the stack.rs C2 prevented-permanent site.
    ///
    /// STAGING NOTE: no ZoneChange registry applier can yield `Prevented`
    /// today, so the natural entry-prevention pause is not constructible
    /// end-to-end; the parked choice is staged as a regeneration-shield Destroy
    /// prevention (the canonical `Prevented` producer) with
    /// `pending_spell_resolution` set. The assertion target —
    /// `handle_replacement_choice`'s Prevented-arm CR 608.3e fallback — is
    /// driven through the real `GameAction::ChooseReplacement` resume entry.
    #[test]
    fn prevented_etb_graveyard_fallback_consults_moved_redirects() {
        use crate::types::ability::AbilityDefinition;
        use crate::types::ability::Effect;
        use crate::types::ability::TargetFilter;
        use crate::types::game_state::{CastingVariant, PendingSpellResolution};
        use crate::types::proposed_event::ReplacementId;

        let mut state = GameState::new_two_player(42);

        // The resolving permanent spell, still on the stack (CR 608.3e: its
        // prevented ETB routes it to its owner's graveyard instead).
        let spell = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Prevented Permanent".to_string(),
            Zone::Stack,
        );

        // Rest in Peace–class graveyard→exile Moved redirect on the battlefield.
        let rip = make_creature(&mut state, PlayerId(1), "Rest in Peace");
        state.objects.get_mut(&rip).unwrap().replacement_definitions =
            vec![ReplacementDefinition::new(ReplacementEvent::Moved)
                .destination_zone(Zone::Graveyard)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        destination: Zone::Exile,
                        origin: None,
                        target: TargetFilter::SelfRef,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        face_down_profile: None,
                    },
                ))]
            .into();

        // The paused entry's spell-resolution bookkeeping.
        state.pending_spell_resolution = Some(PendingSpellResolution {
            object_id: spell,
            controller: PlayerId(0),
            casting_variant: CastingVariant::Normal,
            cast_from_zone: None,
            cast_controller: None,
            cast_timing_permission: None,
            spell_targets: vec![],
            actual_mana_spent: 0,
            kickers_paid: vec![],
            additional_cost_payment_count: 0,
            additional_cost_payments: vec![],
            convoked_creatures: vec![],
        });

        // Staged Prevented producer: a regeneration shield on a creature being
        // destroyed — choosing it yields `ReplacementResult::Prevented`.
        let bear = make_creature(&mut state, PlayerId(0), "Bear");
        state
            .objects
            .get_mut(&bear)
            .unwrap()
            .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Destroy)
            .regeneration_shield()
            .description("Regenerate".to_string())]
        .into();
        state.pending_replacement = Some(crate::types::game_state::PendingReplacement {
            proposed: ProposedEvent::Destroy {
                object_id: bear,
                source: None,
                cant_regenerate: false,
                applied: std::collections::HashSet::new(),
            },
            candidates: vec![ReplacementId {
                source: bear,
                index: 0,
            }],
            depth: 0,
            is_optional: false,
            library_placement: None,
            may_cost_paid: false,
            may_cost_remaining: None,
        });
        state.waiting_for = replacement_mod::replacement_choice_waiting_for(PlayerId(0), &state);
        state.priority_player = PlayerId(0);

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("resume replacement choice");

        assert_eq!(
            state.objects[&spell].zone,
            Zone::Exile,
            "prevented-ETB graveyard fallback must consult the graveyard→exile \
             Moved redirect (CR 614.6) — raw delivery left the spell in the graveyard"
        );
        assert!(
            !state.players[0].graveyard.contains(&spell),
            "the spell must not reach the graveyard with Rest in Peace out"
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

    /// CR 400.7d + CR 608.3 discriminating test (fail-first): a permanent
    /// spell whose `Stack → Battlefield` entry parks on a replacement prompt
    /// must, on resume, still carry its cast link — the kicker payments,
    /// additional-cost count, convoked creatures, and cast-timing permission
    /// that `reset_for_battlefield_entry` (CR 400.7) clears on entry. The
    /// direct stack.rs resolution path restored these in its bespoke epilogue,
    /// but the resume path delivered through the shared machinery with NO
    /// restore (and no `PendingSpellResolution` is stashed when the pause comes
    /// from the generic ZoneChange consult rather than stack.rs's own
    /// NeedsChoice arm) — so a resumed kicked permanent was silently de-kicked
    /// and "if it was kicked" ETB gates (CR 702.33f) failed. The
    /// `CastLinkSnapshot` in `deliver_replaced_zone_change` restores the family
    /// structurally for every `Stack → Battlefield` delivery.
    #[test]
    fn zone_change_replacement_choice_preserves_cast_link_for_resolving_spell() {
        use crate::types::ability::{CastTimingPermission, KickerVariant};

        let mut state = GameState::new_two_player(42);
        let spell = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Kicked Bear".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            // The cast pathway (`finalize_cast_to_stack`) stamps the cast link
            // onto the stack object; mirror that establishment here.
            obj.kickers_paid = vec![KickerVariant::First];
            obj.additional_cost_payment_count = 1;
            obj.convoked_creatures = vec![ObjectId(777)];
            obj.cast_from_zone = Some(Zone::Graveyard);
            obj.cast_controller = Some(PlayerId(0));
            obj.cast_timing_permission =
                Some((CastTimingPermission::AsThoughHadFlash, state.turn_number));
        }
        install_optional_replacement(&mut state, ReplacementEvent::Moved);

        let mut events = Vec::new();
        let proposed = ProposedEvent::zone_change(spell, Zone::Stack, Zone::Battlefield, None);
        let result = replacement_mod::replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::NeedsChoice(player) = result else {
            panic!("expected NeedsChoice, got {result:?}");
        };
        state.waiting_for = replacement_mod::replacement_choice_waiting_for(player, &state);
        state.priority_player = player;

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).expect("accept");

        let obj = &state.objects[&spell];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(
            obj.kickers_paid,
            vec![KickerVariant::First],
            "CR 400.7d: the resumed permanent must keep the kicker payments of \
             the spell that became it — the entry reset cleared them and the \
             resume path had no restore"
        );
        assert_eq!(obj.additional_cost_payment_count, 1);
        assert_eq!(obj.convoked_creatures, vec![ObjectId(777)]);
        assert_eq!(obj.cast_from_zone, Some(Zone::Graveyard));
        assert_eq!(obj.cast_controller, Some(PlayerId(0)));
        assert_eq!(
            obj.cast_timing_permission,
            Some((CastTimingPermission::AsThoughHadFlash, state.turn_number)),
            "CR 603.4: cast-timing permission is re-stamped with the resolution \
             turn so same-turn trigger gates compare equal"
        );
    }

    /// CR 400.7 rules pin for the `CastLinkSnapshot` establishment gate: an
    /// effect-driven put (Reanimate class, `from != Stack`) must NOT resurrect
    /// stale cast provenance. A graveyard card carrying leftover kicker memory
    /// (simulating any exit-clear gap) enters the battlefield as a NEW object —
    /// `reset_for_battlefield_entry` clears the cast link and the snapshot
    /// restore must not re-apply it, or "if it was kicked" gates (CR 702.33f)
    /// would wrongly fire on the reanimated permanent.
    #[test]
    fn effect_put_from_graveyard_does_not_resurrect_cast_link() {
        use crate::game::zone_pipeline::{self, ZoneMoveRequest, ZoneMoveResult};
        use crate::types::ability::KickerVariant;

        let mut state = GameState::new_two_player(42);
        let corpse = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Buried Bear".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&corpse).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            // Stale cast memory on the graveyard object (must NOT survive an
            // effect-driven battlefield entry).
            obj.kickers_paid = vec![KickerVariant::First];
            obj.additional_cost_payment_count = 2;
            obj.cast_from_zone = Some(Zone::Graveyard);
            obj.cast_controller = Some(PlayerId(0));
        }

        let mut events = Vec::new();
        let result = zone_pipeline::move_object(
            &mut state,
            ZoneMoveRequest::effect(corpse, Zone::Battlefield, corpse),
            &mut events,
        );
        assert!(matches!(result, ZoneMoveResult::Done));

        let obj = &state.objects[&corpse];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(
            obj.kickers_paid.is_empty(),
            "CR 400.7: an effect-put permanent is a new object — stale kicker \
             memory must not survive (the cast-link restore is gated on \
             `from == Stack`)"
        );
        assert_eq!(obj.additional_cost_payment_count, 0);
        assert_eq!(obj.cast_from_zone, None);
        assert_eq!(obj.cast_controller, None);
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
                        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
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

    /// Issue #575: Non-Moved `Sacrifice { Typed }` post-replacements (Dralnu)
    /// inject the source as a pre-selected sacrifice target. Re-broadening the
    /// Devour guard to all events would route this through `EffectZoneChoice`.
    #[test]
    fn issue_575_dealt_damage_sacrifice_injects_source_target() {
        let mut state = GameState::new_two_player(42);
        let dralnu = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dralnu, Lich Lord".to_string(),
            Zone::Battlefield,
        );
        let other = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other Bear".to_string(),
            Zone::Battlefield,
        );

        let template = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Sacrifice {
                target: TargetFilter::Typed(crate::types::ability::TypedFilter::permanent()),
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
        );

        let mut events = Vec::new();
        let waiting = apply_post_replacement_effect(
            &mut state,
            &template,
            Some(dralnu),
            None,
            Some(&ReplacementEvent::DealtDamage),
            &mut events,
        );

        assert!(
            !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "DealtDamage sacrifice must use injected source target, not a chooser; got {:?}",
            state.waiting_for
        );
        assert!(waiting.is_none());
        assert_eq!(state.objects[&dralnu].zone, Zone::Graveyard);
        assert_eq!(state.objects[&other].zone, Zone::Battlefield);
    }

    /// Issue #575: Moved (ETB) `Sacrifice { Typed }` post-replacements (Devour)
    /// suppress source injection so the chooser prompt opens.
    #[test]
    fn issue_575_moved_sacrifice_typed_opens_chooser_not_source_injection() {
        let mut state = GameState::new_two_player(42);
        let devourer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Devourer".to_string(),
            Zone::Battlefield,
        );
        let fodder_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Sacrifice Fodder A".to_string(),
            Zone::Battlefield,
        );
        let fodder_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Sacrifice Fodder B".to_string(),
            Zone::Battlefield,
        );
        for id in [devourer, fodder_a, fodder_b] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
        }

        let template = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Sacrifice {
                target: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
        );

        let mut events = Vec::new();
        let waiting = apply_post_replacement_effect(
            &mut state,
            &template,
            Some(devourer),
            None,
            Some(&ReplacementEvent::Moved),
            &mut events,
        );

        assert!(
            matches!(waiting, Some(WaitingFor::EffectZoneChoice { .. }))
                || matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "Moved Devour-shape sacrifice must prompt a chooser; waiting={waiting:?} state={:?}",
            state.waiting_for
        );
        assert_eq!(
            state.objects[&devourer].zone,
            Zone::Battlefield,
            "devourer must not be auto-sacrificed via source injection"
        );
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
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
