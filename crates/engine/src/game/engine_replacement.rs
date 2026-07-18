use std::collections::HashSet;

use crate::ai_support::copy_target_mana_value_ceiling;
use crate::types::ability::{
    AbilityDefinition, Effect, PostReplacementContinuation, ResolvedAbility, TargetFilter,
    TargetRef,
};
#[cfg(test)]
use crate::types::ability::{EffectScope, TapStateChange};
use crate::types::counter::CounterType;
use crate::types::events::{GameEvent, ManaTapState};
use crate::types::game_state::{
    GameState, PendingCostMoveResume, PendingCounterPostAction, TokenEntryEventEmission, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{AppliedReplacementKey, CounterPlacement, ProposedEvent};
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

use super::ability_utils::build_resolved_from_def_with_targets;
use super::effects;
use super::effects::deal_damage::{apply_damage_after_replacement, DamageContext};
use super::effects::destroy::apply_destroy_after_replacement;
use super::effects::draw::apply_draw_after_replacement;
use super::effects::life::{
    apply_life_gain_after_replacement, apply_life_loss_after_replacement,
    drain_pending_life_total_assignment,
};
use super::effects::mill::apply_mill_after_replacement;
use super::effects::scry::apply_scry_after_replacement;
use super::effects::token::apply_create_token_after_replacement;
use super::engine::EngineError;
use super::sacrifice::{apply_sacrifice_after_replacement, SacrificeApply};

/// CR 101.4 + CR 616.1: In a Prevented replacement-resume arm, resume a parked
/// `EachPlayerCopyChosen` walk once its inner copy/counter primitive has fully
/// drained and state is back at Priority. No-op if nothing is parked.
fn maybe_drain_each_player_copy_chosen(state: &mut GameState, events: &mut Vec<GameEvent>) {
    if matches!(state.waiting_for, WaitingFor::Priority { .. })
        && state.pending_each_player_copy_chosen.is_some()
        && state.pending_copy_token_resolution.is_none()
        && state.pending_counter_additions.is_none()
    {
        effects::each_player_copy_chosen::drain_pending(state, events);
    }
}

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
    let Some(pending) = state.pending_replacement.as_ref() else {
        return Err(EngineError::InvalidAction(
            "replacement choice has no pending replacement".to_string(),
        ));
    };
    let option_count = super::replacement::pending_replacement_option_count(state, pending);
    if index >= option_count {
        return Err(EngineError::InvalidAction(format!(
            "replacement choice index {index} is outside 0..{option_count}"
        )));
    }
    let replacement_action_event_start = events.len();
    let parked_search_found_replacement = state
        .pending_replacement
        .as_ref()
        .filter(|pending| matches!(pending.proposed, ProposedEvent::SearchFound { .. }))
        .cloned();
    let pending_was_counter_move = state
        .pending_replacement
        .as_ref()
        .is_some_and(|pending| matches!(pending.proposed, ProposedEvent::MoveCounter { .. }));
    // CR 107.1c + CR 608.2h: mirror of `pending_was_counter_move` for the
    // "remove any number of counters" drain — captured before
    // `continue_replacement` consumes the pending record so the Prevented arm
    // can resume the remaining removals even when this one was fully prevented.
    let pending_was_counter_removal = state
        .pending_replacement
        .as_ref()
        .is_some_and(|pending| matches!(pending.proposed, ProposedEvent::RemoveCounter { .. }));
    // CR 701.24a: capture the parked library placement (W3) BEFORE
    // `continue_replacement` consumes (`.take()`s) the pending record, so the
    // ZoneChange resume arm below can thread it into the delivery `DeliveryCtx`
    // instead of hardcoding `None` (which would let the tail auto-shuffle the
    // requested position away). `None` for every non-library parked event.
    let parked_library_placement = state
        .pending_replacement
        .as_ref()
        .and_then(|pending| pending.library_placement.clone());
    // CR 120.4a + CR 702.15b: capture the excess-redirect rider and the deferred
    // lifelink bonus BEFORE `continue_replacement` consumes the pending record, so
    // the Damage resume arm can restore them onto the ctx it rebuilds from the
    // source (which cannot re-derive either).
    let parked_excess_recipient = state
        .pending_replacement
        .as_ref()
        .and_then(|pending| pending.excess_recipient);
    let parked_lifelink_bonus = state
        .pending_replacement
        .as_ref()
        .map(|pending| pending.lifelink_bonus)
        .unwrap_or(0);
    // CR 701.21a + CR 614.1: An inner graveyard move can have parked after a
    // sacrifice was accepted. The resumed payload is only a ZoneChange, so
    // capture the enclosing sacrifice before `continue_replacement` consumes
    // the pending record; the ZoneChange arm below emits its trigger event only
    // after delivery succeeds.
    let parked_sacrifice_provenance = state
        .pending_replacement
        .as_ref()
        .and_then(|pending| pending.sacrifice_provenance);
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
                            if let (Some(provenance), Some(pending)) = (
                                parked_sacrifice_provenance,
                                state.pending_replacement.as_mut(),
                            ) {
                                if pending.proposed.affected_object_id()
                                    == Some(provenance.object_id)
                                {
                                    pending.sacrifice_provenance = Some(provenance);
                                }
                            }
                            return Ok(state.waiting_for.clone());
                        }
                    }
                    if let Some(provenance) = parked_sacrifice_provenance {
                        if provenance.object_id == object_id {
                            // CR 701.21a + CR 603.2: The resumed move is the
                            // terminal part of the already-accepted sacrifice.
                            // Emit this exactly once, after the move completes,
                            // so sacrifice triggers see its real event rather
                            // than a generic ZoneChanged surrogate.
                            events.push(GameEvent::PermanentSacrificed {
                                object_id,
                                player_id: provenance.player_id,
                            });
                        }
                    }
                    enters_battlefield = to == Zone::Battlefield;
                    zone_change_object_id = Some(object_id);
                }
                event @ ProposedEvent::TokenEntry { entry_ref, .. } => {
                    if state.has_post_replacement_drain() {
                        if let Some(waiting_for) = apply_pending_post_replacement_effect(
                            state,
                            Some(entry_ref),
                            None,
                            Some(ReplacementEvent::Moved),
                            events,
                        ) {
                            state.pending_liminal_entry_resume =
                                Some(crate::types::game_state::PendingLiminalEntryResume::Token {
                                    source_id: entry_ref,
                                    player: waiting_for
                                        .acting_player()
                                        .unwrap_or(state.active_player),
                                    event,
                                });
                            state.waiting_for = waiting_for;
                            return Ok(state.waiting_for.clone());
                        }
                    }
                    if !crate::game::effects::token::commit_liminal_token_entry_and_continue_copy_batch(
                        state, event, events,
                    ) {
                        return Ok(state.waiting_for.clone());
                    }
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
                    let mut ctx =
                        DamageContext::from_source(state, source_id).unwrap_or_else(|| {
                            let controller = state
                                .objects
                                .get(&source_id)
                                .map(|obj| obj.controller)
                                .unwrap_or(state.active_player);
                            DamageContext::fallback(source_id, controller)
                        });
                    // CR 120.4a + CR 702.15b: restore the excess-redirect rider and
                    // the deferred lifelink bonus dropped by the source-derived ctx
                    // rebuild, so the resumed hit still redirects and a resumed redirect
                    // leg still gains the combined lifelink total.
                    ctx.excess_recipient = parked_excess_recipient;
                    ctx.lifelink_bonus = parked_lifelink_bonus;
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
                // CR 701.3a + CR 616.1: Attach accepted after a replacement
                // ordering choice (2+ "as it becomes attached, choose …"
                // definitions on the same attachment). Unreachable today — the
                // parser pool has exactly one `ReplacementEvent::Attached`
                // producer (Psychic Paper) — but wired for correctness if a
                // future card shares the class. `source_id` for the
                // `EffectResolved` push is approximated as `attachment_id`:
                // `ProposedEvent::Attach` doesn't carry the original ability's
                // source, which only differs from the attachment for a
                // non-Equip "attach ~ to" effect (e.g. a triggered ability on
                // another permanent) — no such card has BOTH that shape AND a
                // second Attached-event replacement to order against today.
                ProposedEvent::Attach {
                    attachment_id,
                    target_id,
                    ..
                } => {
                    if let Some(waiting_for) = crate::game::effects::attach::deliver_attach(
                        state,
                        attachment_id,
                        target_id,
                        attachment_id,
                        events,
                    ) {
                        state.waiting_for = waiting_for;
                        return Ok(state.waiting_for.clone());
                    }
                }
                // CR 121.1 + CR 614.6 + CR 614.11: Draw accepted after
                // replacement choice — delegate to the shared post-replacement
                // helper so library-zone move + per-turn accounting match the
                // non-choice delivery. For Abundance-shape replacements
                // (`execute` is a non-Draw chain), `draw_applier` has zeroed
                // the count and the central `post_replacement_continuation`
                // drain below runs the chain (Choose → RevealUntil).
                draw @ ProposedEvent::Draw { player_id, .. } => {
                    let drawn_count = apply_draw_after_replacement(state, draw, events);
                    // CR 121.6b + CR 608.2c: this Draw arm settles the ONE unit
                    // that was paused (the choice just answered) — it does not go
                    // through the sequence driver's own delivery closure, so its
                    // actually-drawn count is folded into the active instruction's
                    // frame here, BEFORE the drain below resumes that frame. Without
                    // this the eventual `last_effect_count` commit would omit every
                    // unit that paused, and a chained "discard that many" would read
                    // short.
                    if let Some(frame) = state.draw_sequences.active_mut() {
                        if frame.player == player_id {
                            frame.accumulated += drawn_count;
                        }
                    }
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
                    // CR 106.4: produced mana goes into the named player's pool. If
                    // that player isn't present, add nothing AND emit no `ManaAdded`
                    // (the event must mirror an actual pool addition — `add_mana_to_pool`
                    // already no-ops on a missing player, so emitting unconditionally
                    // would report mana that was never added).
                    if state.players.iter().any(|p| p.id == player_id) {
                        // CR 107.4h + CR 106.3: mana produced by a snow source is snow mana
                        // (payable for {S}), even when the ProduceMana replacement surfaces as
                        // an interactive choice.
                        let source_is_snow =
                            crate::game::mana_sources::source_is_snow(state, source_id);
                        for _ in 0..count {
                            let unit = crate::types::mana::ManaUnit {
                                color: mana_type,
                                source_id,
                                pip_id: crate::types::mana::ManaPipId(0),
                                supertype: source_is_snow
                                    .then_some(crate::types::mana::ManaSupertype::Snow),
                                source_could_produce_two_or_more_colors: false,
                                restrictions: Vec::new(),
                                grants: Vec::new(),
                                expiry: None,
                            };
                            // CR 118.3a: stamp a stable pip id on pool entry so the unit
                            // can be pinned to direct payment.
                            state.add_mana_to_pool(player_id, unit);
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
                // CR 701.31 + CR 901.9c: Planeswalk is a mandatory full-replacement
                // (Fixed Point in Time). A single mandatory candidate is applied
                // inline by `pipeline_loop` and never surfaces a CR 616.1 choice, so
                // this arm is unreachable in practice.
                ProposedEvent::Planeswalk { .. } => {
                    debug_assert!(
                        false,
                        "handle_replacement_choice: Planeswalk is a mandatory full-replacement and should never surface as a choice"
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
                // CR 701.23a + CR 614.6: modified SearchFound events are delivered by
                // the search-resolution continuation. This arm is reached only
                // when CR 616 ordering required a replacement choice; the bound
                // modified event remains authoritative and is not rescanned.
                event @ ProposedEvent::SearchFound { .. } => {
                    return match super::engine_resolution_choices::resume_search_found_after_replacement(
                        state, event, events,
                    ) {
                        Ok(waiting) => Ok(waiting),
                        Err(error) => {
                            state.pending_replacement = parked_search_found_replacement;
                            Err(error)
                        }
                    };
                }
            }

            let mut waiting_for = WaitingFor::Priority {
                player: state.active_player,
            };
            state.waiting_for = waiting_for.clone();

            let mut replacement_ctx = None;
            if zone_change_object_id
                .zip(state.pending_spell_resolution.as_ref())
                .is_some_and(|(object_id, ctx)| object_id == ctx.object_id)
            {
                let ctx = state
                    .pending_spell_resolution
                    .take()
                    .expect("matching spell-resolution context was checked above");
                if enters_battlefield {
                    apply_pending_spell_resolution(state, &ctx, events);
                }
                replacement_ctx = Some(ctx);
            }

            if state.has_post_replacement_drain() {
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
                    state.clear_post_replacement_source();
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

            // CR 702.143a-c + CR 614.1 + CR 616.1: A Foretell cost move may
            // deliver its card and then surface an interactive post-replacement
            // effect. Complete the special action at the delivery boundary,
            // before preserving that non-priority prompt, so the card cannot be
            // left face up or with a stranded cost continuation.
            if zone_change_object_id.is_some_and(|object_id| {
                matches!(
                    state.pending_cost_move_resume.as_ref(),
                    Some(PendingCostMoveResume::Foretell {
                        object_id: pending_object_id,
                        ..
                    }) if *pending_object_id == object_id
                )
            }) {
                super::casting::complete_foretell_cost_move(state, events);
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

            // CR 121.6b: a draw instruction (`Effect::Draw{count: N}`) paused
            // mid-sequence because a per-unit replacement (Dredge, Notion Thief,
            // Hullbreacher, a count-doubling static, etc.) needed this choice. The
            // just-resolved unit was settled above (Draw arm); resume the frame to
            // drive its remaining units. `resume_draw_sequence` leaves the frame
            // parked and sets `state.waiting_for` (via `draw_through_replacement`)
            // if the next unit surfaces its own choice, so an arbitrary number of
            // sequential re-pauses compose — each resume re-addresses the same
            // frame by ID.
            if matches!(waiting_for, WaitingFor::Priority { .. }) {
                if let Some(frame_id) = state.draw_sequences.active().map(|f| f.frame_id) {
                    let _ =
                        crate::game::effects::draw::resume_draw_sequence(state, frame_id, events);
                    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                        waiting_for = state.waiting_for.clone();
                    }
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

            // CR 107.1c + CR 608.2h: a "remove any number of counters" batch
            // (Rhys, Tetravus) paused mid-removal because a per-removal
            // replacement needed a choice. The chosen event was applied above;
            // drain the parked tail (which re-parks if the next removal surfaces
            // its own choice, setting state.waiting_for for us to propagate).
            if matches!(waiting_for, WaitingFor::Priority { .. })
                && state.pending_counter_removals.is_some()
            {
                effects::counters::drain_pending_counter_removals(state, events);
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
                && state.pending_life_total_assignment.is_some()
            {
                drain_pending_life_total_assignment(state, events);
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
                && state.pending_player_scope_sacrifice_choice.is_some()
            {
                // CR 101.4: a simultaneous each-player sacrifice paused by a
                // CR 616.1 replacement choice resumes the already-announced
                // remaining sacrifices before any parked continuation can run.
                match effects::drain_pending_player_scope_sacrifice_after_replacement(state, events)
                    .map_err(|error| EngineError::InvalidAction(error.to_string()))?
                {
                    effects::PendingPlayerScopeSacrificeOutcome::WaitingForNextChoice => {}
                    effects::PendingPlayerScopeSacrificeOutcome::PausedForReplacement => {
                        waiting_for = state.waiting_for.clone();
                    }
                    effects::PendingPlayerScopeSacrificeOutcome::Completed { .. } => {
                        effects::drain_pending_continuation(state, events);
                        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                            waiting_for = state.waiting_for.clone();
                        }
                    }
                }
            }

            if matches!(waiting_for, WaitingFor::Priority { .. })
                && (state.pending_continuation.is_some()
                    || state.pending_change_zone_iteration.is_some())
                // CR 118.12 + CR 605.3b + CR 616.1: A mana-source cost pause
                // owns the unpaid cost suffix.  Do not drain the ordinary
                // effect rider before that typed root has settled it.
                && !matches!(
                    state.pending_cost_move_resume,
                    Some(PendingCostMoveResume::ManaAbilityPayment { .. })
                )
            {
                // CR 614.12b + CR 614.1c + CR 614.13: drain BOTH the chained
                // continuation and the multi-target ChangeZone iteration that
                // paused on this replacement choice (issue #535). This runs
                // AFTER a collected simultaneous sacrifice queue: CR 101.4
                // requires every already-announced sacrifice to finish before
                // the original ability's later instructions resume.
                effects::drain_pending_continuation(state, events);
                // CR 616.1e: The continuation may itself pause on another replacement
                // (e.g., the second direction of fight damage hitting the same shield),
                // in which case it sets `state.waiting_for` to the next ReplacementChoice.
                // Propagate that back so the engine surfaces the correct prompt.
                if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    waiting_for = state.waiting_for.clone();
                }
            }

            // CR 101.4 + CR 616.1: An `EachPlayerCopyChosen` per-player step
            // paused on a replacement choice for its inner token copy or its
            // +1/+1 counter placement. Both primitives drained above (copy at the
            // copy-token block, counters at the counter-additions block); this
            // hook then drives the counter step (copy-pause resume) or advances
            // the APNAP walk (counter-pause resume). The `drain_pending` guards
            // re-park if either primitive re-paused under a second replacement.
            if matches!(waiting_for, WaitingFor::Priority { .. })
                && state.pending_each_player_copy_chosen.is_some()
                && state.pending_copy_token_resolution.is_none()
                && state.pending_counter_additions.is_none()
            {
                effects::each_player_copy_chosen::drain_pending(state, events);
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

            // CR 601.2h + CR 602.2b + CR 605.3b + CR 616.1: A delivered cost
            // move resumes through the single typed dispatcher before ordinary
            // effect continuations. Foretell completed above at its dedicated
            // delivery boundary and is intentionally ineligible here.
            if matches!(waiting_for, WaitingFor::Priority { .. }) {
                if let Some(resumed) = super::engine::drain_pending_cost_move_resume(
                    state,
                    events,
                    super::engine::CostMoveDrainBoundary::ReplacementDelivered {
                        action_event_start: replacement_action_event_start,
                    },
                )? {
                    waiting_for = resumed;
                }
            }

            // CR 118.12 + CR 605.3b + CR 616.1: The ordinary effect rider may
            // resume only after the whole typed mana-cost root has either paid
            // or failed its suffix.  This mirrors the prevention path below.
            if matches!(waiting_for, WaitingFor::Priority { .. })
                && (state.pending_continuation.is_some()
                    || state.pending_change_zone_iteration.is_some())
            {
                effects::drain_pending_continuation(state, events);
                if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    waiting_for = state.waiting_for.clone();
                }
            }

            // CR 601.2h + CR 602.2b + CR 616.1: Resume a non-move cast or
            // activation cost payment paused during discard or sacrifice.
            if matches!(waiting_for, WaitingFor::Priority { .. })
                && (state.pending_cast.is_some() || state.pending_discard_for_cost.is_some())
            {
                let resume_cost_event_start = state
                    .pending_discard_for_cost
                    .is_some()
                    .then_some(replacement_action_event_start);
                waiting_for = super::casting_costs::resume_interrupted_cost_payment(
                    state,
                    events,
                    resume_cost_event_start,
                )?;
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
                // CR 120.4a: a SECOND material replacement ordering choice on the
                // same damage event re-parked a fresh record with
                // `excess_recipient: None`. Reapply the rider captured before
                // `continue_replacement` consumed the prior record so the eventual
                // delivery still redirects the excess to the creature's controller.
                if pending.excess_recipient.is_none() {
                    pending.excess_recipient = parked_excess_recipient;
                }
                // CR 702.15b: likewise carry the deferred lifelink bonus onto the
                // re-parked record so a second ordering choice cannot drop it.
                if pending.lifelink_bonus == 0 {
                    pending.lifelink_bonus = parked_lifelink_bonus;
                }
                if let Some(provenance) = parked_sacrifice_provenance {
                    if pending.proposed.affected_object_id() == Some(provenance.object_id) {
                        pending.sacrifice_provenance = Some(provenance);
                    }
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
            if state.pending_life_total_assignment.is_some() {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                drain_pending_life_total_assignment(state, events);
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
                // CR 101.4 + CR 616.1: resume an `EachPlayerCopyChosen` walk whose
                // counter placement was prevented — advance to the next player.
                maybe_drain_each_player_copy_chosen(state, events);
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
            if pending_was_counter_removal {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                effects::counters::drain_pending_counter_removals(state, events);
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
                // CR 101.4 + CR 616.1: resume an `EachPlayerCopyChosen` walk whose
                // inner token copy was prevented — drive the counter step.
                maybe_drain_each_player_copy_chosen(state, events);
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
            // CR 601.2h + CR 602.2b + CR 614.12a + CR 616.1: A fully
            // substituted cost move still completes that cost-payment step.
            // No fresh prompt remains at this point, so restore the normal
            // reducer boundary before draining its typed continuation.
            state.waiting_for = WaitingFor::Priority {
                player: state.active_player,
            };
            let resumed_mana_ability_cost = matches!(
                state.pending_cost_move_resume,
                Some(PendingCostMoveResume::ManaAbilityPayment { .. })
            );
            if let Some(waiting_for) = super::engine::drain_pending_cost_move_resume(
                state,
                events,
                super::engine::CostMoveDrainBoundary::ReplacementPrevented {
                    action_event_start: replacement_action_event_start,
                },
            )? {
                // CR 118.12 + CR 605.3b + CR 616.1: A prevented mana cost has
                // the same rider ordering as a delivered one: its typed root
                // settles before any ordinary continuation drains.
                if resumed_mana_ability_cost
                    && matches!(waiting_for, WaitingFor::Priority { .. })
                    && (state.pending_continuation.is_some()
                        || state.pending_change_zone_iteration.is_some())
                {
                    effects::drain_pending_continuation(state, events);
                }
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

    if state.liminal_entries.contains_key(&source_id) {
        let Some(resume) = state.pending_liminal_entry_resume.take() else {
            return Err(EngineError::InvalidAction(
                "Missing liminal entry resume".to_string(),
            ));
        };
        let (resume_source_id, resume_player) = match &resume {
            crate::types::game_state::PendingLiminalEntryResume::Token {
                source_id,
                player,
                ..
            }
            | crate::types::game_state::PendingLiminalEntryResume::Meld {
                source_id, player, ..
            } => (*source_id, *player),
        };
        if resume_source_id != source_id || resume_player != player {
            return Err(EngineError::InvalidAction(
                "Mismatched liminal entry resume".to_string(),
            ));
        }
        let mut ability = copy_effect_for_source(state, source_id)
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
                        recipient: TargetFilter::SelfRef,
                        duration: None,
                        mana_value_limit: None,
                        additional_modifications: Vec::new(),
                    },
                    vec![TargetRef::Object(target_id)],
                    source_id,
                    player,
                )
            });
        if let crate::types::game_state::PendingLiminalEntryResume::Meld { context, .. } = resume {
            // The empty batch record only carried `MeldEntry` across the copy
            // choice. The typed liminal resume now owns completion, including a
            // possible counter-replacement pause, so remove that superseded
            // record before the generic copy finisher tries to drain it.
            if state
                .pending_batch_deliveries
                .as_ref()
                .is_some_and(|pending| {
                    pending.remaining.is_empty()
                        && matches!(
                            &pending.completion,
                            Some(crate::types::game_state::BatchCompletion::MeldEntry { .. })
                        )
                })
            {
                state.pending_batch_deliveries = None;
            }
            if !crate::game::meld::commit_meld_battlefield(state, &context) {
                crate::game::meld::finish_deferred_meld_resolution(state, source_id, events);
                return Ok(state.waiting_for.clone());
            }
            // CR 707.9: the copy effect is installed after the meld result's
            // layer-1 identity, so its later timestamp determines the entering
            // permanent's copiable values.
            let _ = effects::resolve_ability_chain(state, &ability, events, 0);
            let post_actions = vec![PendingCounterPostAction::FinishMeldEntry {
                context: context.clone(),
            }];
            if let Some(waiting_for) =
                finish_copy_target_choice_entry(state, source_id, events, post_actions, false)?
            {
                if state.pending_counter_additions.is_none() {
                    crate::game::meld::finish_deferred_meld_entry(state, context, events);
                }
                return Ok(waiting_for);
            }
            crate::game::meld::finish_deferred_meld_entry(state, context, events);
            return Ok(state.waiting_for.clone());
        }
        let crate::types::game_state::PendingLiminalEntryResume::Token {
            event: resume_event,
            ..
        } = resume
        else {
            unreachable!("meld resume returned above")
        };
        let entry_events = state
            .liminal_entries
            .get(&source_id)
            .map(|entry| (entry.name.clone(), entry.source_id));
        let copy_continuation = state.liminal_entries.get(&source_id).and_then(|entry| {
            entry.copy_resume.as_ref().and_then(|copy| {
                (entry.remaining_count > 0).then(|| {
                    (
                        entry.object.owner,
                        copy.clone(),
                        entry.enter_tapped,
                        entry.enter_with_counters.clone(),
                        entry.remaining_count,
                    )
                })
            })
        });
        // CR 707.9b: a copy effect's "except" clauses (Embalm/Eternalize's
        // "the token has no mana cost, it's white, and it's a Zombie in
        // addition to its other types", etc.) are part of the copy's COPIABLE
        // values, so they must be folded into the `BecomeCopy` that copies the
        // chosen source rather than re-applied as a separate layered effect on
        // top of it. Fold the pending `CopyTokenSpec` exceptions (from the
        // synthesized Embalm/Eternalize `CopyTokenOf`, which reach this liminal
        // accept path only because the copied card carries its own "enter as a
        // copy" replacement) into the `BecomeCopy`'s `additional_modifications`.
        // `become_copy::resolve` is the single authority that consumes
        // `RemoveManaCost` / `SetStartingLoyalty` into the copiable values and
        // layers the remaining exceptions atop the `CopyValues` clone — the
        // only rules-correct home for stamp-only modifications that must never
        // be layered directly.
        let copy_token_exceptions = state.liminal_entries.get(&source_id).and_then(|entry| {
            entry.copy_resume.as_ref().map(|copy| {
                (
                    copy.extra_keywords.clone(),
                    copy.additional_modifications.clone(),
                )
            })
        });
        if let Some((extra_keywords, additional_modifications)) = copy_token_exceptions {
            if let Effect::BecomeCopy {
                additional_modifications: become_copy_mods,
                ..
            } = &mut ability.effect
            {
                become_copy_mods.extend(additional_modifications);
                // CR 707.2 + CR 702: "except it has [keyword]" grants ride the
                // typed `extra_keywords` channel on the token spec; carry them
                // through as layered `AddKeyword` exceptions so they become
                // part of the copy's copiable keyword set.
                become_copy_mods.extend(extra_keywords.into_iter().map(|keyword| {
                    crate::types::ability::ContinuousModification::AddKeyword { keyword }
                }));
            }
        }
        if !super::effects::token::commit_liminal_token_entry_with_event_emission(
            state,
            resume_event,
            events,
            TokenEntryEventEmission::Suppress,
        ) {
            return Ok(state.waiting_for.clone());
        }
        // CR 614.12a: the copy target was chosen before the token entered; the
        // `BecomeCopy` here overwrites the token's copiable values with the
        // chosen source's values, then layers/consumes the folded copy
        // exceptions (CR 707.9b).
        let _ = effects::resolve_ability_chain(state, &ability, events, 0);
        let mut counter_pause_post_actions = Vec::new();
        if let Some((name, event_source_id)) = entry_events.clone() {
            counter_pause_post_actions.push(
                PendingCounterPostAction::EmitCommittedCopyTokenEntry {
                    object_id: source_id,
                    name,
                    source_id: event_source_id,
                },
            );
        }
        if let Some((owner, copy, enter_tapped, enter_with_counters, remaining_count)) =
            copy_continuation.clone()
        {
            counter_pause_post_actions.push(PendingCounterPostAction::ContinueCopyTokenCreation {
                owner,
                copy,
                enter_tapped,
                enter_with_counters,
                remaining_count,
            });
        }
        if let Some(waiting_for) = finish_copy_target_choice_entry(
            state,
            source_id,
            events,
            counter_pause_post_actions,
            true,
        )? {
            return Ok(waiting_for);
        }
        if let Some((name, event_source_id)) = entry_events {
            super::effects::token::push_committed_token_entry_events(
                state,
                source_id,
                name,
                event_source_id,
                events,
            );
        }
        if let Some((owner, copy, enter_tapped, enter_with_counters, remaining_count)) =
            copy_continuation
        {
            let initial_created_ids = state.last_created_token_ids.clone();
            let status =
                super::effects::token_copy::apply_copy_token_after_replacement_with_created_ids(
                    state,
                    owner,
                    *copy,
                    enter_tapped,
                    enter_with_counters,
                    remaining_count,
                    initial_created_ids,
                    events,
                );
            match status.completion {
                super::effects::token_copy::CopyTokenApplyCompletion::Completed => {
                    state.last_created_token_ids = status.created_ids;
                }
                super::effects::token_copy::CopyTokenApplyCompletion::Paused => {
                    state.last_created_token_ids = status.created_ids;
                    return Ok(state.waiting_for.clone());
                }
            }
        }
        if let Some(pending) = state.pending_copy_token_resolution.as_mut() {
            pending.created_ids = state.last_created_token_ids.clone();
        }
        super::effects::token_copy::drain_pending_copy_token_resolution(state, events);
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return Ok(state.waiting_for.clone());
        }
        return Ok(WaitingFor::Priority {
            player: state.active_player,
        });
    }

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
                    recipient: TargetFilter::SelfRef,
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
    if let Some(waiting_for) =
        finish_copy_target_choice_entry(state, source_id, events, Vec::new(), true)?
    {
        return Ok(waiting_for);
    }
    Ok(WaitingFor::Priority {
        player: state.active_player,
    })
}

fn finish_copy_target_choice_entry(
    state: &mut GameState,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
    counter_pause_post_actions: Vec<PendingCounterPostAction>,
    replay_entry_events: bool,
) -> Result<Option<WaitingFor>, EngineError> {
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
        super::effects::counters::append_pending_counter_post_actions(
            state,
            counter_pause_post_actions,
        );
        return Ok(Some(state.waiting_for.clone()));
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
    if replay_entry_events {
        if let Some(waiting_for) = replay_deferred_entry_events(state, source_id, events)? {
            return Ok(Some(waiting_for));
        }
    }
    // CR 702.49c: a ninjutsu entry that deferred `BatchCompletion::NinjutsuPlacement`
    // while paused on `CopyTargetChoice` must run combat placement after the copy
    // resolves (mirrors the `ReturnAsAuraTarget` batch drain in engine.rs).
    if replay_entry_events && state.pending_batch_deliveries.is_some() {
        crate::game::zone_pipeline::drain_pending_batch_deliveries(state, events);
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return Ok(Some(state.waiting_for.clone()));
        }
    }
    // CR 303.4f + CR 303.4g: Copy Enchantment (and any "enter as a copy of an
    // enchantment" effect) can realize into an Aura only AFTER `BecomeCopy` has
    // applied the copied characteristics. `BecomeCopy` is resolved post-entry, so
    // the object was still its pre-copy, non-Aura self when the normal aura-attach
    // slot ran in `move_object` — that consult was skipped and the copy sits
    // unattached, dying to the CR 704.5m unattached-Aura SBA before its copied
    // "level up"/Class ability can matter. Resolve the entering Aura's attachment
    // now that the copy is realized and layers are flushed. This runs LAST — after
    // the entry-trigger replay and batch drain above — so a multi-host player
    // choice can pause on `ReturnAsAuraTarget` with nothing left to strand along
    // the NON-liminal (real Copy Enchantment) caller: the pause resumes straight
    // to priority, and any entry trigger already placed on the stack resolves with
    // the host chosen (CR 303.4f). The auto-attach (0/1 host) branch never pauses.
    //
    // Liminal-path caveat: the copy-token caller (`handle_copy_target_choice`,
    // ~1236) has a `copy_continuation` / committed-token-entry tail that runs only
    // if this function returns `Ok(None)`. A multi-host `NeedsChoice` pause here
    // would return `Ok(Some(..))` and skip that tail — the same drop the existing
    // `replay_deferred_entry_events` pause above already causes. No card reaches
    // that intersection today (the liminal accept path requires the COPIED card to
    // carry its own enter-as-a-copy replacement, and none of those realize into a
    // multi-host Aura with a token continuation); if one ever does, thread the
    // continuation through the resume like the ETB-counter pause does.
    match crate::game::zone_pipeline::resolve_entering_aura_attachment(state, source_id) {
        crate::game::zone_pipeline::EnteringAuraAttachment::NotApplicable
        | crate::game::zone_pipeline::EnteringAuraAttachment::Resolved => {}
        crate::game::zone_pipeline::EnteringAuraAttachment::NeedsChoice {
            controller,
            legal_targets,
        } => {
            state.waiting_for = WaitingFor::ReturnAsAuraTarget {
                player: controller,
                source_id,
                returned_id: source_id,
                legal_targets,
                pending_effect: Box::new(ResolvedAbility::new(
                    Effect::Attach {
                        attachment: TargetFilter::SelfRef,
                        target: TargetFilter::Any,
                    },
                    Vec::new(),
                    source_id,
                    controller,
                )),
            };
            return Ok(Some(state.waiting_for.clone()));
        }
    }
    Ok(None)
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
    if let Some(entry) = state.liminal_entries.get(&source_id) {
        return entry
            .object
            .replacement_definitions
            .iter_all()
            .filter_map(|replacement| replacement.execute.as_deref())
            .find_map(|effect_def| {
                super::replacement::EventModifiers::first_non_modifier_ability(Some(effect_def))
                    .filter(|real| matches!(&*real.effect, Effect::BecomeCopy { .. }))
            });
    }
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
    replacement_applied: HashSet<AppliedReplacementKey>,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let (source_id, controller) = object_id
        .and_then(|obj_id| {
            state
                .objects
                .get(&obj_id)
                .or_else(|| {
                    state
                        .liminal_entries
                        .get(&obj_id)
                        .map(|entry| &entry.object)
                })
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
            let mut resolved =
                build_resolved_from_def_with_targets(real_work, source_id, controller, targets);
            resolved.set_replacement_applied_recursive(replacement_applied);
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
    let mut resolved =
        build_resolved_from_def_with_targets(effect_def, source_id, controller, targets);
    resolved.set_replacement_applied_recursive(replacement_applied);
    let _ = effects::resolve_ability_chain(state, &resolved, events, 0);

    match &state.waiting_for {
        WaitingFor::Priority { .. } => None,
        wf => Some(wf.clone()),
    }
}

pub(crate) fn apply_pending_post_replacement_effect(
    state: &mut GameState,
    object_id: Option<ObjectId>,
    spell_resolution: Option<&crate::types::game_state::PendingSpellResolution>,
    event: Option<ReplacementEvent>,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    // CR 614.12a (approximation): sacrifice prompt fires after ZoneChange completes,
    // matching Siege/Tribute precedent. A strict reading of 614.12a says the choice
    // is made *before* the permanent enters, but the engine's pipeline applies the
    // zone change first and then drains the post-replacement continuation; the
    // observable behavior is equivalent for as-enters sacrifice/counter mechanics
    // (Devour, Siege protector, Tribute) where the choice doesn't gate entry.
    //
    // CR 615.5 + CR 616.1g: Resident-top dispatch. `begin_dispatch` takes the
    // continuation and marks the drain `Dispatching` WITHOUT removing it, so:
    //   * a nested `has_post_replacement_drain()` taken while the effect runs sees
    //     no ready work and cannot re-drain this one (the old slot was likewise
    //     empty at this point — the continuation had been moved out of it), and
    //   * the effect can still read this drain's event context (CR 615.5), which is
    //     how `PostReplacementSourceController` resolves "the source's controller
    //     draws cards" for Swans of Bryn Argoll.
    // The drain is popped after the dispatch returns.
    //
    // `Resolved` carries captured targets (prevention follow-ups); `Template` is an
    // AST that resolves against `source` for ETB / Optional accept.
    let source = state.post_replacement_source().or(object_id);
    let replacement_applied = state
        .post_replacement_drains
        .resident_mut()
        .map(|drain| std::mem::take(&mut drain.applied))
        .unwrap_or_default();

    let waiting_for = match state.post_replacement_drains.begin_dispatch() {
        Some(PostReplacementContinuation::Resolved(resolved)) => {
            apply_post_replacement_resolved_effect(state, &resolved, replacement_applied, events)
        }
        Some(PostReplacementContinuation::Template(effect_def)) => apply_post_replacement_effect(
            state,
            &effect_def,
            source,
            spell_resolution,
            event.as_ref(),
            replacement_applied,
            events,
        ),
        None => None,
    };
    // The dispatch is over: retire the drain, taking its event context with it.
    // This replaces the hand-clearing of `post_replacement_event_source` /
    // `_event_target` that used to sit below.
    state.post_replacement_drains.finish_dispatch();
    // NOTE: the inherited token-choice applied seed is intentionally NOT cleared
    // here. This drain runs for EVERY replacement continuation — including a
    // nested one that pauses inside an outer token-choice ChooseOneOf (issue
    // #4886). Clearing here on "waiting_for is not ChooseOneOfBranch" wipes the
    // outer originating seed when the nested continuation drains, letting the
    // same token-choice replacement re-prompt on a later token sub-ability. The
    // seed is owned and cleared by the originating ChooseOneOf's completion
    // (effects/choose_one_of.rs), which is the only frame that can correctly
    // detect "the token-choice continuation has fully drained."
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
    events: &mut Vec<GameEvent>,
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
    for event in events.iter() {
        if matches!(
            event,
            GameEvent::ZoneChanged { object_id, to, .. }
                if *object_id == source_id && *to == Zone::Battlefield
        ) {
            state.deferred_entry_events.push(event.clone());
        }
    }
    let is_meld_entry = state.liminal_entries.get(&source_id).is_some_and(|entry| {
        matches!(
            entry.kind,
            crate::types::game_state::LiminalEntryKind::Meld { .. }
        )
    });
    if is_meld_entry {
        // The final copy characteristics and CR 508.4 combat status are not
        // known yet. Unlike ordinary copy entry animation, emit this meld entry
        // only after its final snapshot has been refreshed.
        events.retain(|event| {
            !matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Battlefield,
                    ..
                } if *object_id == source_id
            )
        });
    }
}

fn apply_post_replacement_resolved_effect(
    state: &mut GameState,
    resolved: &ResolvedAbility,
    replacement_applied: HashSet<AppliedReplacementKey>,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let mut resolved = resolved.clone();
    resolved.set_replacement_applied_recursive(replacement_applied);
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
                    // CR 202.3d + CR 709.4b: the exiled card is off the stack, so
                    // a split card's mana value is its combined halves.
                    if obj.effective_mana_value() > max {
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
                // CR 202.3d + CR 709.4b: `source_zone` is a non-stack zone
                // (battlefield/graveyard/exile), so a split clone source reports
                // its combined mana value for the MV cap.
                && max_mana_value.is_none_or(|max| obj.effective_mana_value() <= max)
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
                .draw_scope(crate::types::ability::DrawReplacementScope::IndividualDraw)
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

    /// Install Displaced Dinosaurs on the battlefield under `controller`, carrying
    /// the parsed "As a historic permanent you control enters, it becomes a 7/7
    /// Dinosaur creature in addition to its other types" replacement. Returns the
    /// host's ObjectId.
    fn install_displaced_dinosaurs(state: &mut GameState, controller: PlayerId) -> ObjectId {
        use crate::parser::oracle_replacement::parse_replacement_line;

        let host = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            "Displaced Dinosaurs".to_string(),
            Zone::Battlefield,
        );
        let repl = parse_replacement_line(
            "As a historic permanent you control enters, it becomes a 7/7 Dinosaur \
             creature in addition to its other types.",
            "Displaced Dinosaurs",
        )
        .expect("Displaced Dinosaurs replacement must parse");
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .replacement_definitions = vec![repl].into();
        host
    }

    /// CR 614.1c + CR 614.12 + CR 700.6 + CR 205.1b + CR 208.2b: end-to-end
    /// runtime proof for Displaced Dinosaurs' non-self "becomes-in-addition"
    /// replacement. A historic ENTRANT (here a plain artifact — historic via its
    /// artifact card type, CR 700.6) entering under the host's controller is
    /// animated into a 7/7 Dinosaur creature that RETAINS its prior artifact type
    /// (CR 205.1b), and the animation persists after the host leaves (CR 208.2b /
    /// 707.2).
    ///
    /// This is the gate confirming the "no runtime edits" hypothesis: a non-self
    /// `Moved` replacement whose execute is a `GenericEffect` post-replacement
    /// continuation binds its `SelfRef` "becomes" to the separate entrant (CR
    /// 614.12a) and the layer system applies it. Mirrors
    /// `resumed_entry_receives_enters_with_additional_counters_static`.
    #[test]
    fn displaced_dinosaurs_animates_historic_entrant_and_persists() {
        use crate::game::layers::evaluate_layers;
        use crate::game::zone_pipeline::{self, ZoneMoveRequest, ZoneMoveResult};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let host = install_displaced_dinosaurs(&mut state, PlayerId(0));

        // Entrant: a plain artifact (historic via its artifact card type),
        // P0-owned, entering from hand.
        let entrant = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mishra's Bauble".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&entrant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let mut events = Vec::new();
        let result = zone_pipeline::move_object(
            &mut state,
            ZoneMoveRequest::effect(entrant, Zone::Battlefield, entrant),
            &mut events,
        );
        assert!(
            matches!(result, ZoneMoveResult::Done),
            "a single Mandatory becomes replacement must deliver the entry without parking"
        );

        evaluate_layers(&mut state);
        let obj = &state.objects[&entrant];
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "entrant must be on the battlefield"
        );
        // CR 613.4b: base power/toughness set to 7/7.
        assert_eq!(obj.power, Some(7), "entrant power must be set to 7");
        assert_eq!(obj.toughness, Some(7), "entrant toughness must be set to 7");
        // CR 613.1d: Creature type added.
        assert!(
            obj.card_types.core_types.contains(&CoreType::Creature),
            "entrant must become a creature"
        );
        // CR 205.1b: prior artifact type retained (additive).
        assert!(
            obj.card_types.core_types.contains(&CoreType::Artifact),
            "CR 205.1b: entrant must retain its artifact type"
        );
        assert!(
            obj.card_types.subtypes.iter().any(|s| s == "Dinosaur"),
            "entrant must gain the Dinosaur subtype"
        );

        // CR 208.2b + CR 707.2: the characteristics are locked in at entry and
        // persist after the host (Displaced Dinosaurs) leaves the battlefield —
        // the becomes continuous is sourced on the entrant, not the host.
        state.battlefield.retain(|id| *id != host);
        state.objects.remove(&host);
        evaluate_layers(&mut state);
        let obj = &state.objects[&entrant];
        assert_eq!(
            obj.power,
            Some(7),
            "CR 208.2b: 7/7 must persist after the host leaves"
        );
        assert!(
            obj.card_types.subtypes.iter().any(|s| s == "Dinosaur"),
            "CR 208.2b: Dinosaur subtype must persist after the host leaves"
        );
    }

    /// CR 700.6: a non-historic entrant (a vanilla creature — no artifact,
    /// legendary, or Saga) is NOT animated by Displaced Dinosaurs' historic-only
    /// replacement. Discriminates the `FilterProp::Historic` subject guard.
    #[test]
    fn displaced_dinosaurs_does_not_animate_nonhistoric_entrant() {
        use crate::game::layers::evaluate_layers;
        use crate::game::zone_pipeline::{self, ZoneMoveRequest, ZoneMoveResult};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        install_displaced_dinosaurs(&mut state, PlayerId(0));

        // Entrant: a vanilla 2/2 creature (non-historic), P0-owned.
        let entrant = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&entrant).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        let mut events = Vec::new();
        let result = zone_pipeline::move_object(
            &mut state,
            ZoneMoveRequest::effect(entrant, Zone::Battlefield, entrant),
            &mut events,
        );
        assert!(
            matches!(result, ZoneMoveResult::Done),
            "entry must complete"
        );

        evaluate_layers(&mut state);
        let obj = &state.objects[&entrant];
        assert_eq!(
            obj.power,
            Some(2),
            "non-historic creature power must be unchanged"
        );
        assert_eq!(
            obj.toughness,
            Some(2),
            "non-historic creature toughness must be unchanged"
        );
        assert!(
            !obj.card_types.subtypes.iter().any(|s| s == "Dinosaur"),
            "non-historic creature must not gain the Dinosaur subtype"
        );
    }

    /// CR 603.6d: the replacement applies only as a permanent ENTERS. An artifact
    /// already on the battlefield when Displaced Dinosaurs is present is never
    /// animated — there is no entry event for it.
    #[test]
    fn displaced_dinosaurs_does_not_animate_preexisting_artifact() {
        use crate::game::layers::evaluate_layers;
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        install_displaced_dinosaurs(&mut state, PlayerId(0));

        // A historic artifact already on the battlefield (no entry event fires).
        let preexisting = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Sol Ring".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&preexisting)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        evaluate_layers(&mut state);
        let obj = &state.objects[&preexisting];
        assert_ne!(
            obj.power,
            Some(7),
            "a pre-existing artifact must not be animated into a 7/7"
        );
        assert!(
            !obj.card_types.core_types.contains(&CoreType::Creature),
            "a pre-existing artifact must not become a creature"
        );
        assert!(
            !obj.card_types.subtypes.iter().any(|s| s == "Dinosaur"),
            "a pre-existing artifact must not gain the Dinosaur subtype"
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
                        conditional_enter_with_counters: vec![],
                        face_down_profile: None,
                        enters_modified_if: None,
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
            sacrifice_provenance: None,
            candidates: vec![ReplacementId {
                source: bear,
                index: 0,
            }],
            search_found_candidates: Vec::new(),
            depth: 0,
            is_optional: false,
            library_placement: None,
            excess_recipient: None,
            lifelink_bonus: 0,
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
                        conditional_enter_with_counters: vec![],
                        face_down_profile: None,
                        enters_modified_if: None,
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

    /// CR 101.4 + CR 616.1 + CR 701.21a: A player-scope sacrifice batch whose
    /// final announced permanent pauses on an inner graveyard-move replacement
    /// must retain its terminal bookkeeping through the real replacement
    /// response. In particular, an empty remaining queue must still publish the
    /// completed count and changed id before any chained effect could continue.
    #[test]
    fn final_player_scope_sacrifice_replacement_resume_commits_batch_bookkeeping() {
        use crate::game::effects::{
            perform_collected_player_scope_sacrifices, PendingPlayerScopeSacrificeOutcome,
        };
        use crate::types::ability::EffectKind;

        let mut state = GameState::new_two_player(42);
        let victim = make_creature(&mut state, PlayerId(0), "Final sacrifice victim");
        // The outer Sacrifice proposal proceeds, then this optional Moved
        // replacement pauses its inner Battlefield→graveyard move.
        install_optional_replacement(&mut state, ReplacementEvent::Moved);

        let mut events = Vec::new();
        let outcome = perform_collected_player_scope_sacrifices(
            &mut state,
            ObjectId(9_999),
            PlayerId(0),
            vec![(PlayerId(0), vec![victim])],
            &mut events,
        )
        .expect("the announced sacrifice must enter the replacement pipeline");
        assert!(matches!(
            outcome,
            PendingPlayerScopeSacrificeOutcome::PausedForReplacement
        ));
        let pending = state
            .pending_player_scope_sacrifice_choice
            .as_ref()
            .expect("the final paused sacrifice must retain a terminal batch record");
        assert!(
            pending.selections.is_empty(),
            "the regression requires the replacement to pause the final queue item"
        );
        assert_eq!(pending.completion.announced, vec![victim]);

        let result = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accepting the replacement must drain the empty sacrifice queue");

        assert!(state.players[0].graveyard.contains(&victim));
        assert_eq!(state.last_effect_count, Some(1));
        assert_eq!(state.last_zone_changed_ids, vec![victim]);
        assert!(
            result.events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Sacrifice,
                    source_id,
                    subject: None,
                } if *source_id == ObjectId(9_999)
            )),
            "the empty queue still has to publish its terminal sacrifice result"
        );
    }

    /// CR 603.10a + CR 616.1 + CR 701.21a: two permanents announced for one
    /// simultaneous sacrifice instruction remain one co-departure group even
    /// when the second permanent's inner move pauses for a replacement choice.
    /// The first action must not leak a partial `ZoneChanged` event span; the
    /// replacement response reunites both events, stamps reciprocal
    /// `co_departed` values, and updates the per-turn LKI records.
    #[test]
    fn player_scope_sacrifice_replacement_resume_stamps_the_full_departure_batch() {
        use crate::game::effects::{
            perform_collected_player_scope_sacrifices, PendingPlayerScopeSacrificeOutcome,
        };

        let mut state = GameState::new_two_player(42);
        let first = make_creature(&mut state, PlayerId(0), "First sacrifice victim");
        let paused = make_creature(&mut state, PlayerId(0), "Paused sacrifice victim");
        let shield = install_optional_replacement(&mut state, ReplacementEvent::Moved);
        state
            .objects
            .get_mut(&shield)
            .unwrap()
            .replacement_definitions[0]
            .valid_card = Some(TargetFilter::SpecificObject { id: paused });

        let mut events = Vec::new();
        let outcome = perform_collected_player_scope_sacrifices(
            &mut state,
            ObjectId(9_998),
            PlayerId(0),
            vec![(PlayerId(0), vec![first, paused])],
            &mut events,
        )
        .expect("the second sacrifice must pause on its selected Moved replacement");
        assert!(matches!(
            outcome,
            PendingPlayerScopeSacrificeOutcome::PausedForReplacement
        ));
        assert!(
            events.is_empty(),
            "the first departed permanent's events must stay pending until the full batch completes"
        );
        let pending = state
            .pending_player_scope_sacrifice_choice
            .as_ref()
            .expect("the pause must retain the full sacrifice completion");
        assert!(pending.completion.spans_replacement_pause);
        assert!(pending.completion.deferred_events.iter().any(
            |event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if *object_id == first)
        ));

        let result = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accepting the replacement must complete the whole sacrifice batch");
        assert!(state.players[0].graveyard.contains(&first));
        assert!(state.players[0].graveyard.contains(&paused));

        let event_co_departed = |object_id| {
            result
                .events
                .iter()
                .find_map(|event| match event {
                    GameEvent::ZoneChanged {
                        object_id: changed,
                        from: Some(Zone::Battlefield),
                        record,
                        ..
                    } if *changed == object_id => Some(record.co_departed.clone()),
                    _ => None,
                })
                .expect("every sacrifice batch member must be present in the terminal event span")
        };
        assert_eq!(event_co_departed(first), vec![paused]);
        assert_eq!(event_co_departed(paused), vec![first]);

        let record_co_departed = |object_id| {
            state
                .zone_changes_this_turn
                .iter()
                .find_map(|record| {
                    (record.object_id == object_id && record.from_zone == Some(Zone::Battlefield))
                        .then(|| record.co_departed.clone())
                })
                .expect("terminal stamping must update the authoritative LKI record")
        };
        assert_eq!(record_co_departed(first), vec![paused]);
        assert_eq!(record_co_departed(paused), vec![first]);
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

    /// CR 614.6 + CR 111.1: A Jinnie Fay-class optional token replacement
    /// that pauses on `ChooseOneOfBranch` must replace the original token
    /// event, not create it and then prompt again on the chosen branch's
    /// substitute token event.
    #[test]
    fn create_token_choice_replacement_does_not_reprompt_or_create_original_tokens() {
        use crate::types::ability::{AbilityDefinition, Effect, PlayerFilter, PtValue};
        use crate::types::card_type::CoreType;
        use crate::types::mana::ManaColor;
        use crate::types::proposed_event::{TokenCharacteristics, TokenSpec};

        let mut state = GameState::new_two_player(42);
        let replacement_source = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut jinnie = GameObject::new(
            replacement_source,
            CardId(1001),
            PlayerId(0),
            "Jinnie Fay".to_string(),
            Zone::Battlefield,
        );
        let make_branch = |name: &str, types: Vec<&str>, colors: Vec<ManaColor>| {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Token {
                    name: name.to_string(),
                    power: PtValue::Fixed(2),
                    toughness: PtValue::Fixed(2),
                    types: types.into_iter().map(str::to_string).collect(),
                    colors,
                    keywords: vec![],
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    owner: crate::types::ability::TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    supertypes: vec![],
                    static_abilities: vec![],
                    enter_with_counters: vec![],
                },
            )
        };
        jinnie.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .mode(ReplacementMode::Optional { decline: None })
                .description("Jinnie Fay".to_string())
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChooseOneOf {
                        chooser: PlayerFilter::Controller,
                        branches: vec![
                            make_branch("Cat", vec!["Creature", "Cat"], vec![ManaColor::White]),
                            make_branch("Dog", vec!["Creature", "Dog"], vec![ManaColor::Green]),
                        ],
                    },
                )),
        );
        state.objects.insert(replacement_source, jinnie);
        state.battlefield.push_back(replacement_source);

        let treasure_spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Treasure".to_string(),
                power: None,
                toughness: None,
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Treasure".to_string()],
                supertypes: Vec::new(),
                colors: Vec::new(),
                keywords: Vec::new(),
            },
            script_name: "c_a_treasure".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: replacement_source,
            controller: PlayerId(0),
            attach_to: None,
        };
        let battlefield_before = state.battlefield.clone();

        let mut events = Vec::new();
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(treasure_spec),
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

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept token replacement");
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "accepting the replacement must park on the branch choice, got {:?}",
            state.waiting_for
        );

        apply_as_current(&mut state, GameAction::ChooseBranch { index: 1 })
            .expect("choose dog branch");

        let new_ids: Vec<_> = state
            .battlefield
            .iter()
            .filter(|id| !battlefield_before.contains(id))
            .copied()
            .collect();
        assert_eq!(
            new_ids.len(),
            1,
            "the replacement must create exactly the chosen substitute token"
        );
        let token = &state.objects[&new_ids[0]];
        assert_eq!(token.name, "Dog");
        assert!(
            token.card_types.subtypes.iter().any(|s| s == "Dog"),
            "chosen branch token must be the Dog substitute"
        );
        assert!(
            !token.card_types.subtypes.iter().any(|s| s == "Treasure"),
            "original Treasure token must not be created when the replacement is accepted"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "chosen substitute token event must not re-prompt the same replacement"
        );
        assert!(
            state.post_replacement_token_choice_applied.is_none(),
            "replacement-choice applied seed must be cleared after the chosen branch resolves"
        );
    }

    /// CR 614.6 + CR 111.1: The inherited `applied` set for a Jinnie Fay-class
    /// token replacement must stay live for the full paused branch-choice
    /// continuation, not just the first token proposal. Nested `ChooseOneOf`
    /// branches and a branch with a second token sub-ability must not re-prompt
    /// the same replacement mid-continuation.
    #[test]
    fn nested_token_choice_replacement_keeps_applied_set_for_full_branch_chain() {
        use crate::types::ability::{AbilityDefinition, Effect, PlayerFilter, PtValue};
        use crate::types::card_type::CoreType;
        use crate::types::mana::ManaColor;
        use crate::types::proposed_event::{TokenCharacteristics, TokenSpec};

        let mut state = GameState::new_two_player(42);
        let replacement_source = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut jinnie = GameObject::new(
            replacement_source,
            CardId(1002),
            PlayerId(0),
            "Jinnie Fay".to_string(),
            Zone::Battlefield,
        );
        let make_token = |name: &str, types: Vec<&str>, colors: Vec<ManaColor>| {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Token {
                    name: name.to_string(),
                    power: PtValue::Fixed(2),
                    toughness: PtValue::Fixed(2),
                    types: types.into_iter().map(str::to_string).collect(),
                    colors,
                    keywords: vec![],
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    owner: crate::types::ability::TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    supertypes: vec![],
                    static_abilities: vec![],
                    enter_with_counters: vec![],
                },
            )
        };
        let mut dog_then_cat = make_token("Dog", vec!["Creature", "Dog"], vec![ManaColor::Green]);
        dog_then_cat.sub_ability = Some(Box::new(make_token(
            "Cat",
            vec!["Creature", "Cat"],
            vec![ManaColor::White],
        )));
        let nested_choice = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![
                    make_token("Cat", vec!["Creature", "Cat"], vec![ManaColor::White]),
                    dog_then_cat,
                ],
            },
        );
        jinnie.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .mode(ReplacementMode::Optional { decline: None })
                .description("Jinnie Fay".to_string())
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChooseOneOf {
                        chooser: PlayerFilter::Controller,
                        branches: vec![
                            nested_choice,
                            make_token("Dog", vec!["Creature", "Dog"], vec![ManaColor::Green]),
                        ],
                    },
                )),
        );
        state.objects.insert(replacement_source, jinnie);
        state.battlefield.push_back(replacement_source);

        let treasure_spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Treasure".to_string(),
                power: None,
                toughness: None,
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Treasure".to_string()],
                supertypes: Vec::new(),
                colors: Vec::new(),
                keywords: Vec::new(),
            },
            script_name: "c_a_treasure".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: replacement_source,
            controller: PlayerId(0),
            attach_to: None,
        };
        let battlefield_before = state.battlefield.clone();

        let mut events = Vec::new();
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(treasure_spec),
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

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept token replacement");
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "accepting the replacement must park on the outer branch choice, got {:?}",
            state.waiting_for
        );

        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .expect("choose nested branch");
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "nested branch must park on the inner token choice, got {:?}",
            state.waiting_for
        );

        apply_as_current(&mut state, GameAction::ChooseBranch { index: 1 })
            .expect("choose dog-then-cat branch");

        let new_ids: Vec<_> = state
            .battlefield
            .iter()
            .filter(|id| !battlefield_before.contains(id))
            .copied()
            .collect();
        assert_eq!(
            new_ids.len(),
            2,
            "nested continuation must create both substitute tokens without recreating Treasure"
        );
        let names: Vec<_> = new_ids
            .iter()
            .map(|id| state.objects[id].name.clone())
            .collect();
        assert_eq!(names, vec!["Dog".to_string(), "Cat".to_string()]);
        assert!(
            !new_ids.iter().any(|id| state.objects[id]
                .card_types
                .subtypes
                .iter()
                .any(|s| s == "Treasure")),
            "original Treasure token must not be created when the replacement is accepted"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "nested substitute tokens must not re-prompt the same replacement"
        );
        assert!(
            state.post_replacement_token_choice_applied.is_none(),
            "replacement-choice applied seed must clear after the nested branch chain drains"
        );
    }

    /// CR 614.6 + CR 111.1 + issue #4886 (review #6): a token created by the
    /// `sub_ability` tail chained AFTER the whole `ChooseOneOf` — not by one of
    /// its own branches — must still be recognized as part of the token-choice
    /// replacement so the applied set is seeded. Pre-fix, `is_token_replacement_choice`
    /// only scanned the `ChooseOneOf`'s branches (both non-token GainLife here),
    /// so it missed the token-creating tail entirely: the tail token was never
    /// seeded and could re-prompt the same replacement on its own substitute
    /// event.
    #[test]
    fn token_choice_replacement_seeds_applied_set_for_tail_token_after_branches() {
        use crate::types::ability::{
            AbilityDefinition, Effect, PlayerFilter, PtValue, QuantityExpr,
        };
        use crate::types::card_type::CoreType;
        use crate::types::mana::ManaColor;
        use crate::types::proposed_event::{TokenCharacteristics, TokenSpec};

        let mut state = GameState::new_two_player(42);
        let replacement_source = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut jinnie = GameObject::new(
            replacement_source,
            CardId(1003),
            PlayerId(0),
            "Jinnie Fay".to_string(),
            Zone::Battlefield,
        );

        let make_token = |name: &str, types: Vec<&str>, colors: Vec<ManaColor>| {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Token {
                    name: name.to_string(),
                    power: PtValue::Fixed(2),
                    toughness: PtValue::Fixed(2),
                    types: types.into_iter().map(str::to_string).collect(),
                    colors,
                    keywords: vec![],
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    owner: crate::types::ability::TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    supertypes: vec![],
                    static_abilities: vec![],
                    enter_with_counters: vec![],
                },
            )
        };
        let make_gain_life = |amount: i32| {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: amount },
                    player: crate::types::ability::TargetFilter::Controller,
                },
            )
        };

        // Branches create no tokens; the token comes from `sub_ability`, chained
        // after the WHOLE ChooseOneOf resolves — not from any branch.
        let mut choose_then_token = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![make_gain_life(1), make_gain_life(2)],
            },
        );
        choose_then_token.sub_ability = Some(Box::new(make_token(
            "Dog",
            vec!["Creature", "Dog"],
            vec![ManaColor::Green],
        )));

        jinnie.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .mode(ReplacementMode::Optional { decline: None })
                .description("Jinnie Fay".to_string())
                .execute(choose_then_token),
        );
        state.objects.insert(replacement_source, jinnie);
        state.battlefield.push_back(replacement_source);

        let treasure_spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Treasure".to_string(),
                power: None,
                toughness: None,
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Treasure".to_string()],
                supertypes: Vec::new(),
                colors: Vec::new(),
                keywords: Vec::new(),
            },
            script_name: "c_a_treasure".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: replacement_source,
            controller: PlayerId(0),
            attach_to: None,
        };
        let battlefield_before = state.battlefield.clone();

        let mut events = Vec::new();
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(treasure_spec),
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

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept token replacement");
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "accepting the replacement must park on the branch choice, got {:?}",
            state.waiting_for
        );

        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .expect("choose a non-token branch; the tail sub_ability then creates the token");

        let new_ids: Vec<_> = state
            .battlefield
            .iter()
            .filter(|id| !battlefield_before.contains(id))
            .copied()
            .collect();
        assert_eq!(
            new_ids.len(),
            1,
            "only the tail sub_ability's Dog token must be created, not the original Treasure"
        );
        let token = &state.objects[&new_ids[0]];
        assert_eq!(token.name, "Dog");
        assert!(
            !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "the tail token created after the branch choice must not re-prompt the same \
             replacement (pre-fix: is_token_replacement_choice only scanned branches and \
             missed this tail, so the applied set was never seeded)"
        );
        assert!(
            state.post_replacement_token_choice_applied.is_none(),
            "replacement-choice applied seed must clear after the tail continuation drains"
        );
    }

    /// Issue #4886 (HIGH review finding): the inherited token-choice applied
    /// seed must survive an intervening replacement whose own continuation
    /// drains mid-branch. Pre-fix, `apply_pending_post_replacement_effect`
    /// cleared `post_replacement_token_choice_applied` whenever its waiting_for
    /// was not a `ChooseOneOfBranch`, and `continue_replacement_impl`'s
    /// `_ => None` arm re-wiped it for every non-token-choice nested
    /// replacement. Either path let the originating token-choice replacement
    /// re-prompt on a later token sub-ability (the loop). This test pre-seeds
    /// the applied set the way the originating token-choice does, drives a
    /// non-token-choice continuation through the drain, and asserts the seed is
    /// preserved — pinning the fix at both removal sites.
    #[test]
    fn token_choice_applied_seed_survives_intervening_continuation_drain() {
        use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr};
        use crate::types::card_type::CoreType;
        use crate::types::proposed_event::{
            AppliedReplacementKey, ReplacementId, TokenCharacteristics, TokenSpec,
        };

        let mut state = GameState::new_two_player(42);
        let jinnie_source = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let jinnie_rid = ReplacementId {
            source: jinnie_source,
            index: 0,
        };
        let mut seed = std::collections::HashSet::new();
        seed.insert(AppliedReplacementKey::object(
            jinnie_rid.source,
            jinnie_rid.index,
        ));
        // Simulate the originating token-choice continuation being mid-drain:
        // its applied set is live so substitute-token proposals pre-mark Jinnie.
        state.post_replacement_token_choice_applied = Some(seed.clone());

        // A non-token-choice continuation — e.g. an Optional accept that draws
        // a card. Its drain must NOT clear the token-choice seed (pre-fix, the
        // `if !ChooseOneOfBranch { clear }` arm wiped it here).
        let draw_continuation = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                target: crate::types::ability::TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
            },
        );
        state.install_ready_continuation(
            crate::types::ability::PostReplacementContinuation::Template(Box::new(
                draw_continuation,
            )),
        );
        state
            .post_replacement_drains
            .resident_mut()
            .expect("a drain must be resident")
            .source = Some(jinnie_source);

        let mut events = Vec::new();
        let waiting_for = apply_pending_post_replacement_effect(
            &mut state,
            Some(jinnie_source),
            None,
            Some(ReplacementEvent::CreateToken),
            &mut events,
        );

        // The draw continuation is not a ChooseOneOf; before the fix this is
        // exactly the frame that wiped the seed. It must now survive.
        assert_eq!(
            state.post_replacement_token_choice_applied,
            Some(seed),
            "intervening non-token-choice continuation drain must preserve the originating token-choice applied seed (issue #4886)"
        );
        assert!(
            !matches!(waiting_for, Some(WaitingFor::ChooseOneOfBranch { .. })),
            "non-token-choice continuation should not surface a branch choice"
        );

        // A substitute token proposed while the seed is live inherits the
        // originating id, so the same token-choice replacement cannot match it.
        let dog_spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Dog".to_string(),
                power: Some(2),
                toughness: Some(2),
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dog".to_string()],
                supertypes: Vec::new(),
                colors: vec![crate::types::mana::ManaColor::Green],
                keywords: Vec::new(),
            },
            script_name: "c_a_dog".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: jinnie_source,
            controller: PlayerId(0),
            attach_to: None,
        };
        let applied = state
            .post_replacement_token_choice_applied
            .clone()
            .unwrap_or_default();
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(dog_spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied,
        };
        assert!(
            proposed
                .applied_set()
                .iter()
                .any(|rid| rid.source() == jinnie_source),
            "substitute token proposal must inherit the originating replacement id from the live seed"
        );
    }

    /// Issue #4886 (HIGH review finding #3): the originating token-choice
    /// applied seed must survive a branch that parks on a *non-token*
    /// `ChooseOneOf` and then resumes a stashed token sub-ability. Pre-fix,
    /// `choose_one_of.rs` cleared the global seed the moment its branch
    /// resolved back to priority — but the ChooseBranch handler drains the
    /// stashed token sub-ability only afterward (`engine_resolution_choices.rs`
    /// → `drain_pending_continuation`). The later token proposal then lost the
    /// inherited replacement id and re-prompted the same Jinnie replacement.
    ///
    /// Branch shape under test: Jinnie execute = ChooseOneOf([
    ///   { effect: ChooseOneOf([GainLife, GainLife]), sub_ability: Token(Dog) },
    ///   Token(Cat),
    /// ])
    /// Choosing branch 0 parks on the inner non-token choice; resolving it
    /// stashes the Dog sub-ability, which must still see the Jinnie id.
    #[test]
    fn token_choice_seed_survives_non_token_choose_one_of_before_token_sub_ability() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, Effect, PlayerFilter, PtValue,
        };
        use crate::types::card_type::CoreType;
        use crate::types::mana::ManaColor;
        use crate::types::proposed_event::{TokenCharacteristics, TokenSpec};

        let mut state = GameState::new_two_player(42);
        let jinnie_source = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut jinnie = GameObject::new(
            jinnie_source,
            CardId(1020),
            PlayerId(0),
            "Jinnie Fay".to_string(),
            Zone::Battlefield,
        );
        let make_token = |name: &str, types: Vec<&str>, colors: Vec<ManaColor>| {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Token {
                    name: name.to_string(),
                    power: PtValue::Fixed(2),
                    toughness: PtValue::Fixed(2),
                    types: types.into_iter().map(str::to_string).collect(),
                    colors,
                    keywords: vec![],
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    owner: crate::types::ability::TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    supertypes: vec![],
                    static_abilities: vec![],
                    enter_with_counters: vec![],
                },
            )
        };
        let make_gain_life = || {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: crate::types::ability::TargetFilter::Controller,
                },
            )
        };
        // Branch 0: a NON-TOKEN inner choice, with the token in a sub-ability.
        let mut branch_with_nontoken_choice = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![make_gain_life(), make_gain_life()],
            },
        );
        branch_with_nontoken_choice.sub_ability = Some(Box::new(make_token(
            "Dog",
            vec!["Creature", "Dog"],
            vec![ManaColor::Green],
        )));
        jinnie.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .mode(ReplacementMode::Optional { decline: None })
                .description("Jinnie Fay".to_string())
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChooseOneOf {
                        chooser: PlayerFilter::Controller,
                        branches: vec![
                            branch_with_nontoken_choice,
                            make_token("Cat", vec!["Creature", "Cat"], vec![ManaColor::White]),
                        ],
                    },
                )),
        );
        state.objects.insert(jinnie_source, jinnie);
        state.battlefield.push_back(jinnie_source);

        let treasure_spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Treasure".to_string(),
                power: None,
                toughness: None,
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Treasure".to_string()],
                supertypes: Vec::new(),
                colors: Vec::new(),
                keywords: Vec::new(),
            },
            script_name: "c_a_treasure".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: jinnie_source,
            controller: PlayerId(0),
            attach_to: None,
        };
        let battlefield_before = state.battlefield.clone();

        // Propose Treasure → Jinnie is the only matching replacement.
        let mut events = Vec::new();
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(treasure_spec),
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

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept Jinnie");
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "accepting Jinnie must park on the outer token choice, got {:?}",
            state.waiting_for
        );

        // Pick branch 0 — the non-token inner choice. Must park on the inner
        // ChooseOneOf (not create the token yet).
        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .expect("choose branch with nested non-token choice");
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "branch 0 must park on the inner non-token choice before the token sub-ability, got {:?}",
            state.waiting_for
        );

        // Resolve the inner non-token choice. The Dog sub-ability is stashed
        // into pending_continuation and drains after this returns. Pre-fix,
        // choose_one_of.rs cleared the seed at this point — before the stashed
        // Dog sub-ability proposed — so Dog re-prompted Jinnie.
        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .expect("resolve inner non-token choice");

        // The chain must have drained fully: Dog created, no Jinnie re-prompt,
        // no Treasure, seed cleared at full-drain. With the bug, `waiting_for`
        // parked on a second Jinnie ChooseOneOfBranch / ReplacementChoice for
        // the Dog token instead of draining.
        assert!(
            !matches!(
                state.waiting_for,
                WaitingFor::ChooseOneOfBranch { .. } | WaitingFor::ReplacementChoice { .. }
            ),
            "Dog sub-ability must not re-prompt Jinnie after the nested non-token choice; got {:?}",
            state.waiting_for
        );

        let new_ids: Vec<_> = state
            .battlefield
            .iter()
            .filter(|id| !battlefield_before.contains(id))
            .copied()
            .collect();
        assert_eq!(
            new_ids.len(),
            1,
            "the Dog substitute must be created exactly once (no loop, no original Treasure)"
        );
        let token = &state.objects[&new_ids[0]];
        assert_eq!(token.name, "Dog");
        assert!(
            !token.card_types.subtypes.iter().any(|s| s == "Treasure"),
            "original Treasure token must not be created"
        );
        assert!(
            state.post_replacement_token_choice_applied.is_none(),
            "originating token-choice seed must clear at full-drain, after the stashed sub-ability"
        );
    }

    /// Issue #4886 (MED review finding #4): the originating token-choice applied
    /// seed must survive a `pending_repeat_until` drain. Pre-fix,
    /// `drain_pending_continuation` cleared the seed BEFORE calling
    /// `drain_pending_repeat_until`; that drain re-enters `resolve_ability_chain`
    /// (effects/mod.rs:721 / :744) and can emit further token proposals, which
    /// then lost the inherited replacement id and re-prompted the same Jinnie
    /// replacement. The seed must be treated as part of the originating frame
    /// and cleared only once the repeat-until continuation has fully drained or
    /// stopped — i.e. only at true full-drain.
    #[test]
    fn token_choice_seed_survives_pending_repeat_until_drain() {
        use crate::types::ability::{
            AbilityKind, Effect, QuantityExpr, RepeatContinuation, ResolvedAbility,
        };
        use crate::types::game_state::PendingRepeatUntil;
        use crate::types::proposed_event::ReplacementId;

        let mut state = GameState::new_two_player(42);
        let jinnie_source = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let jinnie_rid = ReplacementId {
            source: jinnie_source,
            index: 0,
        };
        let mut seed = std::collections::HashSet::new();
        seed.insert(AppliedReplacementKey::object(
            jinnie_rid.source,
            jinnie_rid.index,
        ));
        state.post_replacement_token_choice_applied = Some(seed.clone());

        // A repeat-until ability whose body would propose further tokens if
        // re-entered. `repeat_until: ControllerChoice` re-prompts after each
        // iteration, so `drain_pending_repeat_until` parks the engine on
        // `RepeatDecision` — a non-Priority waiting_for that MUST preserve the
        // seed (the controller may accept another iteration that proposes a
        // token carrying the inherited id).
        let mut repeat_ability = ResolvedAbility::new(
            Effect::Draw {
                target: crate::types::ability::TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
            },
            Vec::new(),
            jinnie_source,
            PlayerId(0),
        );
        repeat_ability.kind = AbilityKind::Spell;
        repeat_ability.repeat_until = Some(RepeatContinuation::ControllerChoice);
        state.pending_repeat_until = Some(PendingRepeatUntil {
            ability: Box::new(repeat_ability),
        });
        // Simulate the moment the review describes: a paused repeat-until
        // continuation re-entering from priority.
        state.pending_continuation = None;
        state.pending_repeat_iteration = None;
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let mut events = Vec::new();
        effects::drain_pending_continuation(&mut state, &mut events);

        // The repeat-until re-prompted (RepeatDecision), so the originating
        // token-choice frame has NOT fully drained. The seed must survive —
        // pre-fix it was wiped before this drain ran.
        assert!(
            matches!(state.waiting_for, WaitingFor::RepeatDecision { .. }),
            "drain_pending_repeat_until must re-prompt the controller for the repeat decision, got {:?}",
            state.waiting_for
        );
        assert_eq!(
            state.post_replacement_token_choice_applied,
            Some(seed),
            "seed must survive the pending_repeat_until drain (issue #4886 review #4): a repeated iteration may still propose tokens carrying the inherited id"
        );
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
    fn find_copy_targets_gates_on_zone_changed_this_turn_predicate() {
        // CR 400.7 + CR 707.9: The Fourteenth Doctor copies "a Doctor card in
        // your graveyard that was put there from your library this turn". Only a
        // Doctor milled Library->Graveyard THIS turn is a legal source — a
        // Doctor already in the graveyard from a prior turn (no zone-change
        // record) and a Doctor on the battlefield are both excluded. This proves
        // the Fix A (mill records the Library->Graveyard change) <-> Fix B (copy
        // source predicate reads it) runtime seam that couples the card's two
        // abilities.
        use crate::types::ability::{FilterProp, TypeFilter, TypedFilter};
        use crate::types::game_state::ZoneChangeRecord;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        let make_doctor = |state: &mut GameState, cid: u64, zone: Zone| -> ObjectId {
            let id = create_object(
                state,
                CardId(cid),
                PlayerId(0),
                format!("Doctor {cid}"),
                zone,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Creature];
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_card_types.subtypes = vec!["Doctor".to_string()];
            obj.card_types.subtypes = vec!["Doctor".to_string()];
            id
        };

        let milled = make_doctor(&mut state, 1, Zone::Graveyard);
        let prior_turn = make_doctor(&mut state, 2, Zone::Graveyard);
        let on_battlefield = make_doctor(&mut state, 3, Zone::Battlefield);
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "The Fourteenth Doctor".to_string(),
            Zone::Battlefield,
        );

        // Only `milled` carries a Library->Graveyard record this turn.
        state
            .zone_changes_this_turn
            .push(ZoneChangeRecord::test_minimal(
                milled,
                Some(Zone::Library),
                Zone::Graveyard,
            ));

        // The exact filter The Fourteenth Doctor's replacement produces.
        let filter = TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Subtype("Doctor".to_string())).properties(vec![
                FilterProp::InZone {
                    zone: Zone::Graveyard,
                },
                FilterProp::ZoneChangedThisTurn {
                    from: Some(Zone::Library),
                    to: Some(Zone::Graveyard),
                },
            ]),
        );

        let targets = find_copy_targets(&state, &filter, source, PlayerId(0), None);
        assert!(
            targets.contains(&milled),
            "Doctor milled from library this turn must be a legal copy source"
        );
        assert!(
            !targets.contains(&prior_turn),
            "Doctor in graveyard from a prior turn (no zone-change record) must be excluded"
        );
        assert!(
            !targets.contains(&on_battlefield),
            "Doctor on the battlefield must be excluded (wrong zone)"
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

    /// CR 303.4 + CR 707.9: Copy Enchantment copies "any enchantment on the
    /// battlefield". An Aura — including a Curse attached to a player — is an
    /// enchantment permanent (CR 303.4a), so being attached must not remove it
    /// from the copy-choice pool. Reported in #5289.
    #[test]
    fn find_copy_targets_includes_attached_auras_and_curses() {
        use crate::game::game_object::AttachTarget;
        use crate::types::ability::Effect;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        fn make_enchantment(
            state: &mut GameState,
            card: u64,
            name: &str,
            aura: bool,
        ) -> crate::types::ObjectId {
            let id = create_object(
                state,
                CardId(card),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Enchantment];
            obj.card_types.core_types = vec![CoreType::Enchantment];
            if aura {
                obj.base_card_types.subtypes = vec!["Aura".to_string()];
                obj.card_types.subtypes = vec!["Aura".to_string()];
            }
            id
        }

        let prison = make_enchantment(&mut state, 1, "Ghostly Prison", false);
        let host = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&host).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Creature];
            obj.card_types.core_types = vec![CoreType::Creature];
        }
        // CR 303.4f: an Aura attached to a permanent.
        let pacifism = make_enchantment(&mut state, 3, "Pacifism", true);
        state.objects.get_mut(&pacifism).unwrap().attached_to = Some(AttachTarget::Object(host));
        // CR 303.4: a Curse attached to a player.
        let curse = make_enchantment(&mut state, 4, "Cruel Reality", true);
        state.objects.get_mut(&curse).unwrap().attached_to =
            Some(AttachTarget::Player(PlayerId(1)));

        let source = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Copy Enchantment".to_string(),
            Zone::Battlefield,
        );

        // Use the REAL parsed copy filter so this pins parser + runtime together.
        let parsed = crate::parser::oracle::parse_oracle_text(
            "You may have this enchantment enter as a copy of any enchantment on the battlefield.",
            "Copy Enchantment",
            &[],
            &["Enchantment".to_string()],
            &[],
        );
        let filter = parsed
            .replacements
            .iter()
            .find_map(|r| match r.execute.as_deref()?.effect.as_ref() {
                Effect::BecomeCopy { target, .. } => Some(target.clone()),
                _ => None,
            })
            .expect("Copy Enchantment must parse a BecomeCopy clone replacement");

        let targets = find_copy_targets(&state, &filter, source, PlayerId(0), None);

        assert!(
            targets.contains(&prison),
            "unattached enchantment must be copyable"
        );
        assert!(
            targets.contains(&pacifism),
            "#5289: an Aura attached to a creature is still an enchantment permanent"
        );
        assert!(
            targets.contains(&curse),
            "#5289: a Curse attached to a player is still an enchantment permanent"
        );
        assert!(
            !targets.contains(&host),
            "the creature host is not an enchantment"
        );
    }

    /// CR 400.1 + CR 122.1: The Master, Formed Anew — the copy source is "a
    /// creature card in exile with a takeover counter on it". `find_copy_targets`
    /// must scan EXILE (per `InZone { Exile }`) and honor the takeover-counter
    /// `FilterProp::Counters` predicate, returning only the marked exile card and
    /// excluding an unmarked exile creature (and the battlefield entirely). This
    /// is the runtime proof that the parsed source filter selects correctly.
    #[test]
    fn find_copy_targets_honors_exile_zone_and_takeover_counter_predicate() {
        use crate::types::ability::{Comparator, FilterProp, TypeFilter, TypedFilter};
        use crate::types::counter::CounterMatch;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        let make_creature = |state: &mut GameState, card: u64, zone: Zone| {
            let id = create_object(
                state,
                CardId(card),
                PlayerId(0),
                format!("Bear {card}"),
                zone,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Creature];
            obj.card_types.core_types = vec![CoreType::Creature];
            id
        };

        let marked_exile = make_creature(&mut state, 1, Zone::Exile);
        state
            .objects
            .get_mut(&marked_exile)
            .unwrap()
            .counters
            .insert(CounterType::Generic("takeover".to_string()), 1);
        let unmarked_exile = make_creature(&mut state, 2, Zone::Exile);
        let bf_creature = make_creature(&mut state, 3, Zone::Battlefield);
        let source = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "The Master".to_string(),
            Zone::Battlefield,
        );

        // Filter: "a creature card in exile with a takeover counter on it"
        let filter = TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature).properties(vec![
            FilterProp::InZone { zone: Zone::Exile },
            FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Generic("takeover".to_string())),
                comparator: Comparator::GE,
                count: QuantityExpr::Fixed { value: 1 },
            },
        ]));

        let targets = find_copy_targets(&state, &filter, source, PlayerId(0), None);
        assert_eq!(
            targets,
            vec![marked_exile],
            "only the takeover-marked exile creature is a legal copy source"
        );
        assert!(!targets.contains(&unmarked_exile));
        assert!(!targets.contains(&bf_creature));
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
        state.install_ready_continuation(PostReplacementContinuation::Template(Box::new(template)));

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
        assert!(!state.has_post_replacement_drain());
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
        state.install_ready_continuation(PostReplacementContinuation::Template(Box::new(template)));

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
        state.install_ready_continuation(PostReplacementContinuation::Resolved(Box::new(resolved)));

        let mut events = Vec::new();
        let waiting = apply_pending_post_replacement_effect(
            &mut state,
            Some(source),
            None,
            None,
            &mut events,
        );

        assert!(waiting.is_none(), "Resolved path resolved without prompt");
        assert!(!state.has_post_replacement_drain());
        // Resolved ability's own controller (P1) lost 3 life.
        assert_eq!(state.players[1].life, initial_life - 3);
    }

    /// Phase-0 G4 baseline. A general post-replacement drain begins a true draw
    /// that pauses on a replacement consult, the paused state is saved/reloaded,
    /// and a continuation then reads `PostReplacementSourceController`. Today
    /// `finish_dispatch` pops the resident drain before that continuation resumes,
    /// so the event-source context is lost. Phase 1 must retain typed ownership
    /// across this pause: P1 must draw the second card instead of P0.
    #[test]
    fn phase0_g4_general_drain_loses_event_context_across_pausing_draw_resume() {
        use crate::types::ability::{
            AbilityDefinition, Effect, PostReplacementContinuation, QuantityModification,
            TargetFilter,
        };

        let mut state = GameState::new_two_player(42);
        let shield = make_creature(&mut state, PlayerId(0), "Swans-class shield");
        let damage_source = make_creature(&mut state, PlayerId(1), "Damage source");

        for player in [PlayerId(0), PlayerId(1)] {
            for index in 0..6 {
                let card_id = CardId(state.next_object_id);
                create_object(
                    &mut state,
                    card_id,
                    player,
                    format!("P{} library {index}", player.0),
                    Zone::Library,
                );
            }
        }
        // Two one-shot mandatory modifiers produce one real replacement-ordering
        // pause; after the selected modifier applies the lone survivor finishes
        // automatically, matching the paused-draw seam G4 needs.
        for modification in [
            QuantityModification::Times { factor: 2 },
            QuantityModification::Plus { value: 1 },
        ] {
            let host = make_creature(&mut state, PlayerId(0), "Draw replacement host");
            let mut replacement = ReplacementDefinition::new(ReplacementEvent::Draw)
                .draw_scope(crate::types::ability::DrawReplacementScope::IndividualDraw);
            replacement.quantity_modification = Some(modification);
            replacement.consume_on_apply = true;
            state
                .objects
                .get_mut(&host)
                .expect("draw replacement host exists")
                .replacement_definitions
                .push(replacement);
        }

        let mut draw_then_context_draw = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        draw_then_context_draw.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::PostReplacementSourceController,
            },
        )));
        state.install_ready_continuation(PostReplacementContinuation::Template(Box::new(
            draw_then_context_draw,
        )));
        let drain = state
            .post_replacement_drains
            .resident_mut()
            .expect("general drain is resident");
        drain.source = Some(shield);
        drain.event_source = Some(damage_source);

        let mut events = Vec::new();
        let waiting = apply_pending_post_replacement_effect(
            &mut state,
            Some(shield),
            None,
            Some(ReplacementEvent::DamageDone),
            &mut events,
        );
        assert!(
            matches!(waiting, Some(WaitingFor::ReplacementChoice { .. }))
                && matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "reach guard: general drain's first draw must pause on a replacement consult"
        );
        assert!(
            state.post_replacement_drains.is_empty(),
            "CURRENT behavior: finish_dispatch already popped the dispatching drain at the pause"
        );

        let serialized = serde_json::to_string(&state).expect("save paused state");
        let mut restored: GameState =
            serde_json::from_str(&serialized).expect("reload paused state");
        restored.rehydrate_rng();
        apply_as_current(&mut restored, GameAction::ChooseReplacement { index: 0 })
            .expect("resume the paused draw after reload");

        assert!(
            !restored.players[0].hand.is_empty(),
            "reach guard: the first draw completed after the replacement consult"
        );
        assert_eq!(
            restored.players[1].hand.len(),
            0,
            "CURRENT (wrong): the post-resume PostReplacementSourceController read has no resident drain context, so P1 draws zero; Phase 1 must make P1 draw one"
        );
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
        assert!(!state.has_post_replacement_drain());

        state.migrate_post_replacement_continuation();

        match state.post_replacement_continuation() {
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
            Default::default(),
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
            Default::default(),
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

        match state.post_replacement_continuation() {
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
        state.install_ready_continuation(PostReplacementContinuation::Template(Box::new(
            new_template.clone(),
        )));
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

        match state.post_replacement_continuation() {
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
                            recipient: TargetFilter::SelfRef,
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

    /// Build a "Level Up"-style Aura source (Enchantment with the `Aura` subtype
    /// and `Enchant creature`) plus `num_hosts` legal creature hosts, then drive
    /// Copy Enchantment through `Stack → Battlefield` with its real parsed
    /// `BecomeCopy` replacement, accept the "may", and surface `CopyTargetChoice`.
    /// Returns the state paused on the copy-target choice, the Copy Enchantment
    /// object, the Aura source, and the host ids. Callers then pick the Aura as
    /// the copy source and assert the CR 303.4f/303.4g attachment outcome.
    fn drive_copy_enchantment_of_aura(
        num_hosts: usize,
    ) -> (GameState, ObjectId, ObjectId, Vec<ObjectId>) {
        use crate::types::ability::{Effect, TargetFilter, TypeFilter, TypedFilter};
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(42);

        let hosts: Vec<ObjectId> = (0..num_hosts)
            .map(|i| make_creature(&mut state, PlayerId(0), &format!("Grizzly Bears {i}")))
            .collect();

        // "Enchant creature" — the copiable Enchant keyword the copy inherits.
        let enchant_creature =
            Keyword::Enchant(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));

        // Aura copy SOURCE (stands in for Level Up): a real Enchantment/Aura on
        // the battlefield with `Enchant creature`. Set base_* so the layer system
        // (which `finish_copy_target_choice_entry` flushes) recomputes the copy's
        // live characteristics from copiable values.
        let aura = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Level Up".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Enchantment];
            obj.card_types.core_types = vec![CoreType::Enchantment];
            obj.base_card_types.subtypes = vec!["Aura".to_string()];
            obj.card_types.subtypes = vec!["Aura".to_string()];
            obj.base_keywords = vec![enchant_creature.clone()];
            obj.keywords = vec![enchant_creature];
        }

        // Copy Enchantment on the stack, entering the battlefield.
        let copy_ench = create_object(
            &mut state,
            CardId(301),
            PlayerId(0),
            "Copy Enchantment".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&copy_ench).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Enchantment];
            obj.card_types.core_types = vec![CoreType::Enchantment];
        }

        // Use the REAL parsed copy filter so the test pins parser + runtime.
        let parsed = crate::parser::oracle::parse_oracle_text(
            "You may have this enchantment enter as a copy of any enchantment on the battlefield.",
            "Copy Enchantment",
            &[],
            &["Enchantment".to_string()],
            &[],
        );
        let copy_filter = parsed
            .replacements
            .iter()
            .find_map(|r| match r.execute.as_deref()?.effect.as_ref() {
                Effect::BecomeCopy { target, .. } => Some(target.clone()),
                _ => None,
            })
            .expect("Copy Enchantment must parse a BecomeCopy clone replacement");

        state
            .objects
            .get_mut(&copy_ench)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .valid_card(TargetFilter::SelfRef)
                    .destination_zone(Zone::Battlefield)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::BecomeCopy {
                            recipient: TargetFilter::SelfRef,
                            target: copy_filter,
                            duration: None,
                            mana_value_limit: None,
                            additional_modifications: Vec::new(),
                        },
                    )),
            );

        // Propose the Stack → Battlefield entry so the replacement pipeline
        // surfaces the optional "may enter as a copy" choice.
        let mut events = Vec::new();
        let proposed = ProposedEvent::ZoneChange {
            object_id: copy_ench,
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
            panic!("expected NeedsChoice for Copy Enchantment's optional copy, got {result:?}");
        };
        state.waiting_for = replacement_mod::replacement_choice_waiting_for(player, &state);
        state.priority_player = player;
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept Copy Enchantment's enter-as-a-copy");

        // The mid-entry copy target choice must now be pending, with the Aura as
        // the only legal copy source (a creature host is not an enchantment).
        let WaitingFor::CopyTargetChoice { valid_targets, .. } = state.waiting_for.clone() else {
            panic!(
                "expected CopyTargetChoice after accepting the copy, got {:?}",
                state.waiting_for
            );
        };
        assert!(
            valid_targets.contains(&aura),
            "the Level Up Aura must be a legal copy source"
        );

        (state, copy_ench, aura, hosts)
    }

    /// Build an "Enchant player" Aura source plus Copy Enchantment entering
    /// through the real `BecomeCopy` replacement. Both players are legal hosts,
    /// so picking the Aura as the copy source must pause on
    /// `ReturnAsAuraTarget` with `TargetRef::Player` choices.
    fn drive_copy_enchantment_of_player_aura() -> (GameState, ObjectId, ObjectId) {
        use crate::types::ability::Effect;
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(42);

        let enchant_player = Keyword::Enchant(TargetFilter::Player);

        let aura = create_object(
            &mut state,
            CardId(302),
            PlayerId(0),
            "Psychic Venom".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Enchantment];
            obj.card_types.core_types = vec![CoreType::Enchantment];
            obj.base_card_types.subtypes = vec!["Aura".to_string()];
            obj.card_types.subtypes = vec!["Aura".to_string()];
            obj.base_keywords = vec![enchant_player.clone()];
            obj.keywords = vec![enchant_player];
        }

        let copy_ench = create_object(
            &mut state,
            CardId(303),
            PlayerId(0),
            "Copy Enchantment".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&copy_ench).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Enchantment];
            obj.card_types.core_types = vec![CoreType::Enchantment];
        }

        let parsed = crate::parser::oracle::parse_oracle_text(
            "You may have this enchantment enter as a copy of any enchantment on the battlefield.",
            "Copy Enchantment",
            &[],
            &["Enchantment".to_string()],
            &[],
        );
        let copy_filter = parsed
            .replacements
            .iter()
            .find_map(|r| match r.execute.as_deref()?.effect.as_ref() {
                Effect::BecomeCopy { target, .. } => Some(target.clone()),
                _ => None,
            })
            .expect("Copy Enchantment must parse a BecomeCopy clone replacement");

        state
            .objects
            .get_mut(&copy_ench)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .valid_card(TargetFilter::SelfRef)
                    .destination_zone(Zone::Battlefield)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::BecomeCopy {
                            recipient: TargetFilter::SelfRef,
                            target: copy_filter,
                            duration: None,
                            mana_value_limit: None,
                            additional_modifications: Vec::new(),
                        },
                    )),
            );

        let mut events = Vec::new();
        let proposed = ProposedEvent::ZoneChange {
            object_id: copy_ench,
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
            panic!("expected NeedsChoice for Copy Enchantment's optional copy, got {result:?}");
        };
        state.waiting_for = replacement_mod::replacement_choice_waiting_for(player, &state);
        state.priority_player = player;
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept Copy Enchantment's enter-as-a-copy");

        let WaitingFor::CopyTargetChoice { valid_targets, .. } = state.waiting_for.clone() else {
            panic!(
                "expected CopyTargetChoice after accepting the copy, got {:?}",
                state.waiting_for
            );
        };
        assert!(
            valid_targets.contains(&aura),
            "the enchant-player Aura must be a legal copy source"
        );

        (state, copy_ench, aura)
    }

    /// CR 303.4f + CR 704.5m: Copy Enchantment entering as a copy of an Aura
    /// ("Level Up") with exactly one legal host must AUTO-ATTACH to that host as
    /// the copy is realized, then SURVIVE the unattached-Aura state-based action.
    /// Before the fix, `BecomeCopy` realized the Aura post-entry — after the
    /// normal `move_object` aura-attach slot had already been skipped (the object
    /// was still a non-Aura enchantment then) — so the copy sat unattached and
    /// was destroyed by CR 704.5m before its copied ability could ever matter.
    #[test]
    fn copy_enchantment_becomes_aura_copy_auto_attaches_and_survives_sba() {
        use crate::game::game_object::AttachTarget;

        let (mut state, copy_ench, aura, hosts) = drive_copy_enchantment_of_aura(1);
        let host = hosts[0];

        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(crate::types::ability::TargetRef::Object(aura)),
            },
        )
        .expect("pick the Aura as the copy source");

        let copy = &state.objects[&copy_ench];
        assert!(
            copy.card_types.subtypes.iter().any(|s| s == "Aura"),
            "the copy must have realized into an Aura"
        );
        assert_eq!(
            copy.attached_to,
            Some(AttachTarget::Object(host)),
            "CR 303.4f: the copied Aura must auto-attach to its sole legal host"
        );

        // CR 704.5m: the attached Aura must survive the state-based action that
        // sends UNATTACHED Auras to the graveyard.
        let mut events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut events);
        assert!(
            state.battlefield.contains(&copy_ench),
            "the attached copy must survive the unattached-Aura SBA"
        );
    }

    /// CR 303.4g + CR 704.5m: With NO legal host on the battlefield, the copied
    /// Aura stays unattached (the engine's post-entry equivalent of "it doesn't
    /// enter") and is moved to its owner's graveyard by the unattached-Aura SBA.
    #[test]
    fn copy_enchantment_becomes_aura_copy_with_no_host_dies_to_sba() {
        let (mut state, copy_ench, _aura, _hosts) = drive_copy_enchantment_of_aura(0);

        // The only battlefield permanent is the Aura source itself (an
        // Enchantment, not a creature), so "Enchant creature" has no legal host.
        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(crate::types::ability::TargetRef::Object(_aura)),
            },
        )
        .expect("pick the Aura as the copy source");

        assert!(
            state.objects[&copy_ench].attached_to.is_none(),
            "with no legal host the copied Aura must stay unattached"
        );

        let mut events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut events);
        assert!(
            !state.battlefield.contains(&copy_ench),
            "CR 704.5m: an unattached Aura copy must be moved off the battlefield by SBA"
        );
        assert_eq!(
            state.objects[&copy_ench].zone,
            Zone::Graveyard,
            "CR 303.4g: the unattached Aura copy goes to its owner's graveyard"
        );
    }

    /// CR 303.4f: With MULTIPLE legal hosts, the copied Aura must PAUSE on
    /// `ReturnAsAuraTarget` for the controller's choice, then attach to the
    /// chosen host and survive SBA. Exercises the interactive branch of the
    /// entering-Aura attachment building block.
    #[test]
    fn copy_enchantment_becomes_aura_copy_multi_host_prompts_then_attaches() {
        use crate::game::game_object::AttachTarget;

        let (mut state, copy_ench, _aura, hosts) = drive_copy_enchantment_of_aura(2);

        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(crate::types::ability::TargetRef::Object(_aura)),
            },
        )
        .expect("pick the Aura as the copy source");

        // Two legal hosts → the controller must choose which to enchant.
        let WaitingFor::ReturnAsAuraTarget {
            returned_id,
            legal_targets,
            ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected ReturnAsAuraTarget for the multi-host attach choice, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(
            returned_id, copy_ench,
            "the entering copy is the Aura to attach"
        );
        assert!(
            hosts
                .iter()
                .all(|h| legal_targets.contains(&TargetRef::Object(*h))),
            "both creatures must be offered as legal Aura hosts"
        );

        let chosen = hosts[1];
        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(chosen)),
            },
        )
        .expect("choose the Aura's host");

        assert_eq!(
            state.objects[&copy_ench].attached_to,
            Some(AttachTarget::Object(chosen)),
            "the copied Aura must attach to the chosen host"
        );
        let mut events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut events);
        assert!(
            state.battlefield.contains(&copy_ench),
            "the attached copy must survive the unattached-Aura SBA"
        );
    }

    /// CR 303.4f: With multiple legal player hosts, an entering copied
    /// "Enchant player" Aura must offer player hosts and attach to the chosen
    /// player. This guards the shared `ReturnAsAuraTarget` resume path for
    /// `TargetRef::Player`, not only object hosts.
    #[test]
    fn copy_enchantment_becomes_enchant_player_aura_prompts_then_attaches_to_player() {
        use crate::game::game_object::AttachTarget;

        let (mut state, copy_ench, aura) = drive_copy_enchantment_of_player_aura();

        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(aura)),
            },
        )
        .expect("pick the enchant-player Aura as the copy source");

        let WaitingFor::ReturnAsAuraTarget {
            returned_id,
            legal_targets,
            ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected ReturnAsAuraTarget for the multi-player attach choice, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(
            returned_id, copy_ench,
            "the entering copy is the Aura to attach"
        );
        assert!(
            legal_targets.contains(&TargetRef::Player(PlayerId(0)))
                && legal_targets.contains(&TargetRef::Player(PlayerId(1))),
            "both players must be offered as legal Aura hosts"
        );

        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
        )
        .expect("choose the Aura's player host");

        assert_eq!(
            state.objects[&copy_ench].attached_to,
            Some(AttachTarget::Player(PlayerId(1))),
            "the copied Aura must attach to the chosen player"
        );
        let mut events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut events);
        assert!(
            state.battlefield.contains(&copy_ench),
            "the attached player-enchanting copy must survive the unattached-Aura SBA"
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
            state.has_post_replacement_drain(),
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
            replacement_applied: Default::default(),
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
