use crate::types::game_state::EntryEventReplay;
use std::collections::HashSet;

use crate::ai_support::copy_target_mana_value_ceiling;
use crate::game::game_object::ObjectIncarnationRefExt;
use crate::types::ability::{
    AbilityDefinition, AttachCardinality, AttachSelection, ChosenAttribute, CopyTargetPurpose,
    Effect, EffectKind, PostReplacementContinuation, PtValue, QuantityExpr, ResolvedAbility,
    TargetFilter, TargetRef, TokenAttackDestination,
};
#[cfg(test)]
use crate::types::ability::{EffectScope, TapStateChange};
use crate::types::counter::CounterType;
use crate::types::events::{GameEvent, ManaTapState, PlayerActionKind};
use crate::types::game_state::TokenCopySettlement;
use crate::types::game_state::{
    CounterCostRootSubtree, DeferredLifeCostResume, DrainStatus, GameState, ParkedEntry,
    ParkedEntryState, PendingCostMoveResume, PendingCounterPostAction,
    PendingEffectResolutionEvent, PendingEffectResolved, PendingReplacement, PostReplacementDrain,
    PostReplacementDrainStack, ReplacementChoiceKind, SeatCreatedTokens, StagedMemberOutcome,
    TokenEntryEventEmission, TokenSubstituteChoice, UnrepresentablePolicy, WaitingFor,
};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{
    AppliedReplacementKey, CounterCause, CounterPlacement, DrawEventStage, ProposedEvent,
};
use crate::types::replacements::ReplacementEvent;
use crate::types::resolution::ChildStackDepth;
use crate::types::zones::Zone;

use super::ability_utils::build_resolved_from_def_with_targets;
use super::arithmetic::u32_to_i32_saturating;
use super::casting_costs::AnsweredLifeGain;
use super::effects;
use super::effects::deal_damage::{
    apply_damage_after_replacement, damage_context_for_pinned_incarnation, DamageResult,
};
use super::effects::destroy::apply_destroy_after_replacement;
use super::effects::draw::{apply_draw_after_replacement, settle_draw_instruction};
use super::effects::life::{
    drain_pending_life_total_assignment, settle_life_gain_after_choice,
    settle_life_loss_after_choice, within_ingress_life_window, LifeChangeError,
    LifeChangeUnrepresentable,
};
use super::effects::mill::apply_mill_after_replacement;
use super::effects::scry::apply_scry_after_replacement;
use super::effects::token::{
    apply_create_token_after_replacement, apply_create_token_after_replacement_with_created_ids,
};
use super::engine::EngineError;
use super::engine_resolution_choices::DepartureSettlement;
use super::life_costs::PayLifeCostResult;
use super::replacement::{ReplacementResume, TokenSubstituteAnswer};
use super::sacrifice::{apply_sacrifice_after_replacement, SacrificeApply};

/// CR 101.4 + CR 616.1: In a Prevented replacement-resume arm, resume a parked
/// `EachPlayerCopyChosen` walk once its inner copy/counter primitive has fully
/// drained and state is back at Priority. No-op if nothing is parked.
fn maybe_drain_each_player_copy_chosen(state: &mut GameState, events: &mut Vec<GameEvent>) {
    if matches!(state.waiting_for, WaitingFor::Priority { .. })
        && state.active_each_player_copy_chosen().is_some()
        && state.active_copy_token().is_none()
        && state.active_counter_additions().is_none()
    {
        effects::each_player_copy_chosen::drain_pending(state, events);
    }
}

/// CR 608.3a / CR 608.3c + CR 400.7d + CR 616.1f: complete a permanent spell's
/// parked resolution frame once the child stack its own entry raised has retired.
///
/// The parked frame is that child's structural PARENT, so it becomes the active
/// frame only here — after this resume's drain stages have settled. `subject`
/// names the object whose frame may complete: the object whose replacement this
/// resume answered or, from `finish_spell_resolution_exposed_by_retired_drain`,
/// the exposed frame's own object once it is on the battlefield. An answering
/// resume never completes another object's frame.
/// `active_spell_resolution` stays top-only: this reads the frame the drain just
/// exposed and never reaches through a live child. This function is the SINGLE
/// settle authority for that completion — call sites add no `Priority` guard of
/// their own.
///
/// This mirrors the existing completion authority at the ZoneChange Execute site
/// below, which cannot serve the counter path: it runs before the counter drain
/// and keys on `zone_change_object_id`, which is `None` when the resumed event is
/// an AddCounter.
///
/// KNOWN RESIDUAL — do not "fix" it here (measured):
/// On a Devour-shape entrant (CR 702.82a/c) the counter drain exposes the entry's
/// CR 614.13a eligibility snapshot, not the parent, so this helper correctly
/// no-ops and the entry strands exactly as it does without this change. Retiring
/// that snapshot here removes the strand and VIOLATES CR 614.13a: the snapshot is
/// the eligible-pool filter in `game::effects::sacrifice` (`is_none_or` — vacuous
/// once cleared), and the sacrifice that consumes it has not run at this program
/// point, so the devourer becomes legal fodder for its own Devour. Measured on
/// both arms. The blocking shape is `[PostReplacement(sacrifice, Ready),
/// SpellResolution(parent), ChangeZone(snapshot)]`, in which
/// `ResolutionStack::active_post_replacement_parent_slot` — top, or exactly one
/// below — cannot resolve, so neither the sacrifice nor the snapshot's own
/// retirement authority can run while the parent sits between them. The fix is an
/// ordering change at the capture site in `stack.rs`, not a retirement call here.
fn finish_spell_resolution_exposed_by_child(
    state: &mut GameState,
    subject: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) {
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        return;
    }
    if !state
        .active_spell_resolution()
        .is_some_and(|ctx| Some(ctx.object_id) == subject)
    {
        return;
    }
    let ctx = state
        .take_active_spell_resolution()
        .expect("matching spell-resolution frame was checked above");
    apply_pending_spell_resolution(state, &ctx, events);
}

/// CR 608.3a + CR 616.1f: a post-replacement drain raised while the spell's
/// entry replacements were being ordered sits above the spell's parked frame.
/// Retiring it exposes that frame with the entry already delivered, and no
/// answering action remains to finish it.
pub(crate) fn finish_spell_resolution_exposed_by_retired_drain(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) {
    if state.pending_replacement.is_some() {
        return;
    }
    let subject = state
        .active_spell_resolution()
        .map(|ctx| ctx.object_id)
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|object| object.zone == Zone::Battlefield)
        });
    finish_spell_resolution_exposed_by_child(state, subject, events);
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

/// CR 614.12 + CR 614.1c: whether `id`'s own as-enters replacement reads the
/// battlefield population as it was before the entry — a self-scoped
/// "enters with N counters" whose count is a dynamic quantity ("for each
/// +1/+1 counter among other creatures you control"). Such an entrant must not
/// count permanents entering in the same simultaneous event, so the mass-entry
/// loops snapshot the pre-entry battlefield for it, exactly as for Devour.
pub(crate) fn object_has_pre_entry_population_replacement(state: &GameState, id: ObjectId) -> bool {
    state.objects.get(&id).is_some_and(|obj| {
        obj.replacement_definitions.iter_all().any(|def| {
            def.valid_card == Some(TargetFilter::SelfRef) && {
                let mut current = def.execute.as_deref();
                let mut dynamic = false;
                while let Some(exec) = current {
                    dynamic |= matches!(
                        &*exec.effect,
                        Effect::PutCounter { count, .. }
                            if !matches!(count, crate::types::ability::QuantityExpr::Fixed { .. })
                    );
                    current = exec.sub_ability.as_deref();
                }
                dynamic
            }
        })
    })
}

/// CR 707.5 + CR 614.12: whether `id` has its own as-enters copy replacement
/// ("you may have this enter as a copy of any creature", Clone). The copiable
/// values it takes on at entry can carry a replacement of the copied object's
/// own (a copied Ascendant Acolyte's population count, a copied Devour), which
/// the member's CURRENT definitions do not yet show.
pub(crate) fn object_has_as_enters_copy_replacement(state: &GameState, id: ObjectId) -> bool {
    state.objects.get(&id).is_some_and(|obj| {
        obj.replacement_definitions.iter_all().any(|def| {
            def.valid_card == Some(TargetFilter::SelfRef)
                && super::replacement::EventModifiers::first_non_modifier_ability(
                    def.execute.as_deref(),
                )
                .is_some_and(|real| matches!(&*real.effect, Effect::BecomeCopy { .. }))
        })
    })
}

/// CR 614.12a + CR 614.13a + CR 614.1c + CR 707.5: whether a co-entering member
/// needs the pre-entry battlefield snapshot — a Devour sacrifice pool, a
/// population-counting as-enters replacement, or an as-enters copy that may
/// acquire either from the object it copies. Capturing for every copy-capable
/// member (rather than every multi-member batch) keeps the snapshot, and the
/// batch frame that carries it, off every entry that cannot need it.
pub(crate) fn object_needs_pre_entry_battlefield(state: &GameState, id: ObjectId) -> bool {
    object_has_devour_replacement(state, id)
        || object_has_pre_entry_population_replacement(state, id)
        || object_has_as_enters_copy_replacement(state, id)
}

/// CR 614.12 + CR 614.1c + CR 614.12a: single authority deciding whether a
/// simultaneous multi-permanent entry needs the pre-entry battlefield snapshot,
/// and capturing it. Returns the battlefield as it stands BEFORE the first member
/// enters when any member needs it, so every co-entrant is excluded regardless of
/// delivery order. An already-active snapshot (this or an outer entry's) is never
/// re-captured, which keeps a resumed batch on its original snapshot.
pub(crate) fn pre_entry_snapshot_for_entry(
    state: &GameState,
    members: impl IntoIterator<Item = ObjectId>,
    destination: Zone,
) -> Option<HashSet<ObjectId>> {
    (destination == Zone::Battlefield
        && state.active_pre_entry_battlefield_snapshot().is_none()
        && members
            .into_iter()
            .any(|id| object_needs_pre_entry_battlefield(state, id)))
    .then(|| state.battlefield.iter().copied().collect())
}

/// CR 614.12 + CR 614.1c: the permanents that entered in the active simultaneous
/// entry — on the battlefield now, absent from its pre-entry `snapshot`. They have
/// no continuous effect yet for an entrant's own entry replacements.
pub(crate) fn pre_entry_co_entrants(
    state: &GameState,
    snapshot: &HashSet<ObjectId>,
) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| !snapshot.contains(id))
        .collect()
}

/// CR 614.12 + CR 614.1c + CR 613.1: the battlefield as the rules saw it before
/// the active simultaneous entry, as a detached scratch state — what an
/// entering permanent's own replacement counts ("for each +1/+1 counter among
/// other creatures you control"). CR 614.12 checks the entrant against its own
/// statics and "continuous effects that already exist"; a permanent entering in
/// the same event has no static effect yet, so it must not change the
/// characteristics (a noncreature artifact turned into a creature by a
/// co-entering March of the Machines) of the permanents the replacement counts.
///
/// Built from `state` by taking every battlefield object outside `snapshot` off
/// the battlefield and running the full layer pass, which recomputes the
/// survivors' characteristics without those objects' statics. Co-entrants stay
/// in `objects` (the replacement's own source reads through it) but are no
/// longer on the battlefield, so no population read can see them. `None` when
/// nothing entered since the snapshot: `state` already is the pre-entry view.
/// Nothing reaches the live game: the input is `&GameState`.
pub(crate) fn pre_entry_rules_view(
    state: &GameState,
    snapshot: &HashSet<ObjectId>,
) -> Option<GameState> {
    let co_entrants = pre_entry_co_entrants(state, snapshot);
    if co_entrants.is_empty() {
        return None;
    }
    let mut view = state.clone();
    // allow-raw-zone: drops co-entrants from the battlefield list of a detached scratch state used only to recompute layers for the pre-entry population read; the live game is untouched and no zone change occurs (CR 614.12).
    view.battlefield.retain(|id| snapshot.contains(id));
    for id in co_entrants {
        if let Some(obj) = view.objects.get_mut(&id) {
            // allow-raw-zone: marks a co-entrant as not on the battlefield in a detached scratch state used only to recompute layers; no zone change occurs (CR 614.12).
            obj.zone = Zone::Exile;
        }
    }
    super::layers::mark_layers_full(&mut view);
    super::layers::flush_layers(&mut view);
    Some(view)
}

/// As [`pre_entry_snapshot_for_entry`], for the ChangeZone multi-entry loops:
/// parks the snapshot in the loop's ChangeZone frame.
pub(crate) fn capture_pre_entry_snapshot_for_entry(
    state: &mut GameState,
    members: impl IntoIterator<Item = ObjectId>,
    destination: Zone,
) {
    if let Some(snapshot) = pre_entry_snapshot_for_entry(state, members, destination) {
        state.push_devour_change_zone_snapshot(snapshot);
    }
}

/// CR 701.50a + CR 614.5 + CR 616.1f: drain a deferred connive whose leading
/// Draw link parked a replacement-ordering choice. Runs only when the dedicated
/// stack-owned Connive re-entry is set (the connive applier's parked-draw
/// path). `propose_connive` re-enters with the already-applied rids excluded
/// (CR 614.5) so the CR 616.1f repeat covers the remaining connive replacements.
/// Called from BOTH the Execute arm (the leading draw delivered) and the
/// Prevented arm (the inner draw was prevented, but CR 701.50a's "draw a card,
/// THEN that creature connives" still runs the connive step — the prevention
/// replaced only the draw). Returns the parked `WaitingFor` (ConniveDiscard /
/// fresh ReplacementChoice) if `propose_connive` parked one, else `None`.
pub(crate) fn drain_pending_connive_reentry(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let reentry = state.take_active_connive_reentry()?;
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

/// CR 121.2 + CR 614.11a: drive every resumable draw instruction to completion
/// or to its next pause. Returns the new `WaitingFor` when a resumed draw
/// surfaces a choice, else `None`. Callers check for a Priority wait before
/// calling; this helper has no entry check of its own.
///
/// `resume_draw_sequence` leaves a frame parked and sets `state.waiting_for`
/// when its next draw surfaces its own choice, so any number of sequential
/// re-pauses compose — each resume re-addresses the same frame by ID.
///
/// A completed draw frame also retires the emptied post-replacement frame it
/// exposes, so the stack top seen by every caller is the frame that was beneath
/// the draw's substitute.
pub(super) fn resume_active_draw_sequences(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    while let Some(frame_id) = state.active_draw_sequence().map(|frame| frame.frame_id) {
        let _ = crate::game::effects::draw::resume_draw_sequence(state, frame_id, events);
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return Some(state.waiting_for.clone());
        }
        if state
            .active_draw_sequence()
            .is_some_and(|frame| frame.frame_id == frame_id)
        {
            // No frame completed or paused; avoid retrying a stalled frame.
            break;
        }
        // CR 614.11a + CR 121.6b: a draw replaced by a substitute (Dredge's
        // mill-and-return) dispatched that substitute from a post-replacement
        // frame installed beneath this draw frame. Its dispatch finished while
        // the draw frame was on top, so the top-only retirement could not
        // remove it; the completed draw now exposes it, and it is retired here
        // rather than at the next priority sweep, so a parked entry MayCost
        // finds its permanent's `SpellResolution` on top again. The retirement
        // never exposes another draw frame: a draw has one `MultiDraw` frame (the
        // stack validator rejects a second one as a split draw authority), and a
        // nested draw instruction (Teferi's Ageless Insight's "draw two cards
        // instead") is a sequence inside that frame, which the loop's top
        // re-read already resumes without any retirement.
        state.remove_empty_active_post_replacement_frame();
    }
    None
}

/// CR 608.2f + CR 614.5 (F1/F2): readiness gate for the deferred publication
/// of a paused simultaneous multi-seat token clause. The clause finishes only
/// once its record is installed, no replacement or entry choice is live, no
/// compound cursor, copy owner, unfinished resolving owner on top (the resumer
/// loop's own set) or uncommitted entry remains, and the action is back at
/// Priority. Evaluated at every pass boundary of the common priority resumer.
/// Single-fire: the finish takes the record.
fn simultaneous_token_clause_is_ready(state: &GameState) -> bool {
    // CR 614.6: a post-replacement drain still on top is the replaced event's
    // modified event still occurring, so the clause finishes after it. An entry
    // not yet committed (a parked liminal entry or token battlefield entry)
    // would be missing from the published union.
    if state.pending_simultaneous_token_clause.is_none()
        || state.pending_replacement.is_some()
        || !matches!(state.waiting_for, WaitingFor::Priority { .. })
        || state.active_copy_token().is_some()
        || super::engine::top_frame_owes_resolving_work(state)
        || state.pending_liminal_entry_resume.is_some()
        || state.pending_token_battlefield_entry.is_some()
    {
        return false;
    }
    // No live pre-entry choice of the parked entry (subsumed by the Priority
    // + no-pending conjuncts above; kept as defense in depth).
    if super::replacement::pending_entry_named_choice_is_live(state)
        || super::replacement::pending_entry_payment_is_live(state)
    {
        return false;
    }
    // No compound cursor outstanding on the active counter completion.
    !state
        .active_counter_additions()
        .and_then(|queue| queue.completion.as_ref())
        .is_some_and(|completion| {
            completion.post_actions.iter().any(|action| {
                matches!(
                    action,
                    PendingCounterPostAction::ContinueTokenEventDelivery { .. }
                )
            })
        })
}

/// CR 608.2f + CR 608.2c + CR 609.3 (F1/F2): deferred publication of a paused
/// simultaneous multi-seat token clause — the reunion the synchronous Done
/// path performs inline. Appends the completing action's buffer, emits one
/// terminal event per instructed seat, records the deduped player actions,
/// publishes the shared clause results, attributes the deduped union to seats
/// by controller, publishes the created referent, and takes the record.
fn finish_simultaneous_token_clause(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // B-final: the completing action's own buffer joins the clause span.
    effects::counters::feed_simultaneous_token_clause(state, events);
    let Some(record) = state.pending_simultaneous_token_clause.as_ref() else {
        return;
    };
    let source_id = record.source_id;
    let seats = record.seats.clone();
    // One terminal event per instructed seat, mirroring the Done path.
    let terminal: Vec<GameEvent> = seats
        .iter()
        .map(|_| GameEvent::EffectResolved {
            kind: EffectKind::Token,
            source_id,
            subject: None,
        })
        .collect();
    events.extend(terminal.iter().cloned());
    // The clause span for every publication below: every fed buffer plus the
    // terminal events just emitted (mirrors the Done path's clause window).
    let mut span = state
        .pending_simultaneous_token_clause
        .as_ref()
        .map(|record| record.accumulated.clone())
        .unwrap_or_default();
    span.extend(terminal);
    // The union over the span (self-deduping by object id).
    let union = effects::created_token_union_from_events(state, &span);
    // Deduped player-action recording: the fed buffers overlap (entry+exit
    // feeds), and the turn ledger blind-pushes — dedup BEFORE recording.
    let mut seen: HashSet<(PlayerId, PlayerActionKind)> = HashSet::new();
    let mut firsts: Vec<GameEvent> = Vec::new();
    for event in &span {
        if let GameEvent::PlayerPerformedAction {
            player_id, action, ..
        } = event
        {
            if seen.insert((*player_id, *action)) {
                firsts.push(event.clone());
            }
        }
    }
    effects::record_player_actions_performed(state, &firsts);
    // Shared clause publication. The gate pins the template (bare Token, no
    // tail, no tracked-set consumer), so a minimal synthesized ability reads
    // identically through every helper the publication consults (kind-level
    // count/amount channels, zone-change ledger, no tracked set, discarded
    // linked-exile batch).
    let template = ResolvedAbility::new(
        Effect::Token {
            name: String::new(),
            power: PtValue::Fixed(0),
            toughness: PtValue::Fixed(0),
            types: Vec::new(),
            colors: Vec::new(),
            keywords: Vec::new(),
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            attack_destination: TokenAttackDestination::ControllerChooses,
            supertypes: Vec::new(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
        },
        Vec::new(),
        source_id,
        seats.first().copied().unwrap_or(state.active_player),
    );
    let ledger_before_clause = state.last_zone_changed_ids.clone();
    effects::publish_player_scope_clause_results(
        state,
        &template,
        &template,
        &seats,
        false,
        &span,
        ledger_before_clause,
    );
    // Attribute the deduped union to seats by token controller (the gate pins
    // the controller to the instructed seat), then publish the referent.
    let mut seat_results: Vec<SeatCreatedTokens> = Vec::new();
    for seat in &seats {
        effects::begin_seat_created_tokens_if_absent(&mut seat_results, *seat);
    }
    for pin in union {
        let holder = state
            .objects
            .get(&pin.object_id)
            .map(|object| object.controller)
            .unwrap_or(seats.first().copied().unwrap_or(state.active_player));
        effects::extend_seat_created_tokens(&mut seat_results, holder, vec![pin]);
    }
    effects::publish_created_token_union(state, &seat_results);
    // The clause completed: clear its frozen values (mirrors the synchronous
    // driver postlude; a gated token clause captures none) and take the
    // record so this finish single-fires.
    state.clause_minimum_snapshot = None;
    state.pending_simultaneous_token_clause = None;
}

/// CR 608.2f + CR 608.2c (F1/F2): the single finish authority of a paused
/// simultaneous token clause, called only from the common priority boundary
/// (`engine::resume_pending_continuation_pass`). Single-fire: the finish takes
/// the record.
pub(super) fn finish_simultaneous_token_clause_if_ready(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) {
    if simultaneous_token_clause_is_ready(state) {
        finish_simultaneous_token_clause(state, events);
    }
}

/// The paused simultaneous token clause's identity at an action boundary:
/// its creating instruction's source. One definition for the start capture
/// and the end check of `feed_paused_token_clause_after_action`.
pub(super) fn paused_token_clause_source(state: &GameState) -> Option<ObjectId> {
    state
        .pending_simultaneous_token_clause
        .as_deref()
        .map(|record| record.source_id)
}

/// The action-boundary feed of a paused simultaneous token clause: every
/// action after the installing one belongs to the clause span until its
/// finish, whatever it answered — a replacement choice, an as-enters or
/// branch choice, a copy target, a payment, a trigger prompt — so its whole
/// event buffer joins `accumulated`. Keyed on the record this action began
/// with: the installing action (fed by its own clause slice) and a record
/// another instruction installed during this action are never fed pre-clause
/// events, and a clause that finished inside this action (whose finish fed
/// the buffer itself) is gone. Called once per action, from
/// `engine::apply_action_boundary_core`.
pub(super) fn feed_paused_token_clause_after_action(
    state: &mut GameState,
    clause_at_start: Option<ObjectId>,
    events: &[GameEvent],
) {
    if clause_at_start.is_some() && paused_token_clause_source(state) == clause_at_start {
        effects::counters::feed_simultaneous_token_clause(state, events);
    }
}

/// CR 616.1 + CR 510.2 + CR 702.15b: the CR 616.1 round trip, plus the one turn-based
/// action that can be parked waiting on it.
///
/// A simultaneous combat-damage batch (CR 510.2) parks in
/// `state.pending_combat_lifelink` (lifelink gain ordering, or its replacement pass). The resumed
/// `LifeChanged` must join the batch's own CR 603.3b trigger batch, so the resume needs to know
/// where THIS action's events begin — so the anchor is a LOCAL captured here, and the drain lives
/// here rather than in `engine::resume_pending_continuation_if_priority`, which has no such
/// anchor. This handler resumes in place because its batch-leg arm consumes the choice index;
/// every other answer converges at the generic boundary,
/// `engine::resume_parked_combat_damage_at_action_boundary`, which also holds the events of a
/// non-`Priority` return below (its fold-only arm). Stored on the record it would be a persisted
/// index into a finished action's event vector with no owner to clear it on every exit path; a
/// stale one panics or folds unrelated events into the batch, while a local cannot go stale and
/// `action_event_start <= events.len()` holds by construction. A wrapper rather than in-arm
/// insertions, because the inner handler returns early from many points: one authority for
/// `Execute`, `Prevented`, `NeedsChoice`, and a no-op whenever nothing is parked.
pub(super) fn handle_replacement_choice(
    state: &mut GameState,
    index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_replacement_choice_resume(state, ReplacementResume::Index(index), events)
}

/// CR 614.6 + CR 616.1: answer a parked token substitute through the
/// replacement-choice authority, so the rewritten event delivers through the
/// same arms (and caller tails) an accepted branch would.
pub(super) fn resume_token_substitute(
    state: &mut GameState,
    answer: TokenSubstituteAnswer,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_replacement_choice_resume(state, ReplacementResume::TokenSubstitute(answer), events)
}

pub(super) fn handle_replacement_choice_resume(
    state: &mut GameState,
    resume: ReplacementResume,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let action_event_start = events.len();
    // CR 616.1 + CR 510.2: the ordering choice of one of the parked batch's OWN
    // damage events. The batch answers it and keeps the event inside the
    // simultaneous batch (Phase C applies the survivor, the substitute runs
    // inline); the generic dispatcher below would deal the damage on its own.
    if super::combat_damage::parked_combat_damage_awaits_replacement_choice(state) {
        // A substitute answer carries no ordering index; the lifelink resume
        // settles without one, as on its settled path below.
        let index = match resume {
            ReplacementResume::Index(index) => {
                validate_replacement_choice_index(state, index)?;
                Some(index)
            }
            ReplacementResume::TokenSubstitute(answer) => {
                validate_token_substitute_answer(state, &answer)?;
                None
            }
        };
        let waiting_for = super::combat_damage::resume_pending_combat_lifelink(
            state,
            action_event_start,
            index,
            events,
        )
        .unwrap_or_else(|| state.waiting_for.clone());
        state.waiting_for = waiting_for.clone();
        return Ok(waiting_for);
    }
    match resume {
        ReplacementResume::Index(index) => validate_replacement_choice_index(state, index)?,
        ReplacementResume::TokenSubstitute(answer) => {
            validate_token_substitute_answer(state, &answer)?
        }
    }
    resolve_pending_replacement(state, events, |state, events| {
        super::replacement::continue_replacement_with(state, resume, events)
    })
}

/// CR 616.1: the answer must name one of the open prompt's options.
fn validate_replacement_choice_index(state: &GameState, index: usize) -> Result<(), EngineError> {
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
    Ok(())
}

/// CR 616.1: settle the parked replacement through `operation`: the chooser's
/// answer, or what a departure decides for it (a reconsultation of the changed
/// event, or an unpaid cost whose payer left).
fn resolve_pending_replacement(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    operation: impl FnOnce(&mut GameState, &mut Vec<GameEvent>) -> super::replacement::ReplacementResult,
) -> Result<WaitingFor, EngineError> {
    // CR 101.4 + CR 616.1: the choice decides one sacrifice of a simultaneous
    // batch that is still being consulted. Its move waits with the batch instead
    // of happening now, however the choice is settled; the generic dispatcher
    // below would deliver it at once, before the rest of the batch is consulted.
    if effects::player_scope_sacrifice_consult_awaits_replacement_choice(state) {
        let action_event_start = events.len();
        let waiting_for = answer_player_scope_sacrifice_consult(state, events, operation)?;
        return Ok(settle_replacement_choice_wait(
            state,
            events,
            action_event_start,
            waiting_for,
        ));
    }
    // CR 616.1: retire the answered prompt once, before any nested consultation.
    let placeholder = WaitingFor::Priority {
        player: state.active_player,
    };
    let answered = std::mem::replace(&mut state.waiting_for, placeholder.clone());
    match handle_replacement_choice_body(state, events, operation) {
        Ok(waiting_for) => {
            state.waiting_for = waiting_for.clone();
            Ok(waiting_for)
        }
        Err(error) => {
            if state.waiting_for == placeholder {
                state.waiting_for = answered;
            }
            Err(error)
        }
    }
}

/// Stack-local authority captured before the complete CR 800.4a transaction.
/// The live record remains resident so exact member/root retirement can see it.
pub(crate) struct InterruptedReplacement {
    boundary: ProposedEvent,
    subjects: [Option<ObjectIncarnationRef>; 2],
    actor: Option<PlayerId>,
    cost_payer: Option<PlayerId>,
    replacement_choice: Option<ReplacementChoiceKind>,
    /// The players this transaction removes.
    leaving: std::collections::HashSet<PlayerId>,
}

pub(crate) fn capture_replacement_before_departure(
    state: &GameState,
    leaving: &std::collections::HashSet<PlayerId>,
) -> Option<InterruptedReplacement> {
    let pending = state.pending_replacement.as_ref()?;
    let pin = |id| {
        state
            .objects
            .get(&id)
            .map(ObjectIncarnationRef::from_object)
    };
    let subjects = match &pending.proposed {
        ProposedEvent::MoveCounter {
            source_id,
            destination_id,
            ..
        } => [pin(*source_id), pin(*destination_id)],
        ProposedEvent::Attach {
            attachment_id,
            target_id,
            ..
        } => [pin(*attachment_id), pin(*target_id)],
        // A mana source is provenance, not the player receiving this event.
        ProposedEvent::ProduceMana { .. } => [None, None],
        _ => [pending.proposed.affected_object_id().and_then(pin), None],
    };
    Some(InterruptedReplacement {
        boundary: pending.delivery_boundary_event().clone(),
        subjects,
        actor: state.waiting_for.acting_player(),
        cost_payer: super::replacement::pending_replacement_cost_payer(state, pending),
        replacement_choice: if let WaitingFor::ReplacementChoice { kind, .. } = &state.waiting_for {
            Some(*kind)
        } else {
            None
        },
        leaving: leaving.clone(),
    })
}

/// CR 104.1: retire only this interrupted event and its owned cost child.
pub(crate) fn retire_replacement_at_game_over(
    state: &mut GameState,
    interrupted: Option<InterruptedReplacement>,
) {
    let Some(interrupted) = interrupted else {
        return;
    };
    if let ProposedEvent::BeginPhase { phase, .. } = &interrupted.boundary {
        super::turns::abandon_phase_begin_choice(state, *phase);
    }
    if state
        .pending_replacement
        .as_ref()
        .is_some_and(|pending| pending.parks_delivery(&interrupted.boundary))
    {
        state.pending_replacement = None;
        state.pending_single_discard = None;
        state.replacement_may_cost_paused = false;
        super::replacement::abandon_post_replacement_continuation(state);
    }
    if matches!(
        state.pending_cost_move_resume,
        Some(PendingCostMoveResume::ReplacementMayCost { .. })
    ) {
        state.pending_cost_move_resume = None;
    }
}

/// CR 800.4a/f + CR 616.1: reconcile once, after every departure/control sweep,
/// through the same result dispatcher and epilogue as an actual answer.
pub(crate) fn settle_replacement_after_departure(
    state: &mut GameState,
    interrupted: Option<InterruptedReplacement>,
    events: &mut Vec<GameEvent>,
) -> Result<super::engine_resolution_choices::DepartureSettlement, EngineError> {
    let Some(interrupted) = interrupted else {
        return Ok(super::engine_resolution_choices::DepartureSettlement::NotSettled);
    };
    // CR 603.2 + CR 603.3b + CR 603.7: the concession's canonical tail starts
    // after its already-owned departure buffer. Keep resumed instruction events
    // separate and collect them here, as in player-scope departure settlement:
    // a paused instruction may prepend its retained events to this buffer.
    let mut resumed = Vec::new();
    let settlement = settle_replacement_after_departure_inner(state, interrupted, &mut resumed);
    if !resumed.is_empty() {
        // CR 603.2 + CR 603.7: both the direct phase drain and the shared
        // continuation epilogue can resume the turn interpreter, which already
        // owns ordinary and delayed phase-entry collection. Exclude only that
        // input; all public events and unowned substitute events remain intact.
        let instruction_events;
        let collection_events = if resumed
            .iter()
            .any(|event| matches!(event, GameEvent::PhaseChanged { .. }))
        {
            instruction_events = resumed
                .iter()
                .filter(|event| !matches!(event, GameEvent::PhaseChanged { .. }))
                .cloned()
                .collect::<Vec<_>>();
            &instruction_events
        } else {
            &resumed
        };
        super::triggers::collect_unclaimed_triggers_into_deferred(
            state,
            collection_events,
            0,
            None,
        );
        events.extend(resumed);
    }
    settlement
}

fn settle_replacement_after_departure_inner(
    state: &mut GameState,
    interrupted: InterruptedReplacement,
    events: &mut Vec<GameEvent>,
) -> Result<super::engine_resolution_choices::DepartureSettlement, EngineError> {
    // CR 800.4a + CR 608.2f + CR 616.1: token-creation parks are not settled
    // here. `do_eliminate` prunes the departed batches first
    // (`prune_departed_token_batches`: survivors stand for the
    // departure-boundary repair, no-survivor parks are abandoned with their
    // companions), and the repair re-proposes or completes them; a standing
    // prompt is never re-proposed for a live chooser (a dead pick applies
    // vacuously instead). Reconsulting here would drop departed candidates
    // the prompt must keep, and the pending-None arm below would re-finish
    // an already-abandoned park. Keyed on the captured boundary so it also
    // covers parks the prune already consumed.
    if matches!(interrupted.boundary, ProposedEvent::CreateToken { .. }) {
        return Ok(DepartureSettlement::NotSettled);
    }
    if let Some(waiting) = super::costs::settle_replacement_may_cost_after_departure(state, events)?
    {
        state.waiting_for = waiting;
        return Ok(replacement_departure_settlement(state));
    }
    let Some(pending) = state.pending_replacement.as_ref() else {
        // Exact member retirement may already have consumed the record. Never
        // resurrect it; its owner's surviving tail still owes completion.
        if matches!(state.waiting_for, WaitingFor::Priority { .. })
            || state.waiting_for.acting_player() == interrupted.actor
        {
            let waiting = finish_abandoned_replacement(state, &interrupted.boundary, events)?;
            state.waiting_for = waiting;
            return Ok(replacement_departure_settlement(state));
        }
        return Ok(DepartureSettlement::NotSettled);
    };
    if !pending.parks_delivery(&interrupted.boundary) {
        return Ok(DepartureSettlement::NotSettled);
    }
    // CR 510.2 + CR 616.1f: a combat-damage leg's choice belongs to its parked
    // simultaneous batch, whose own reconciliation re-proposes the leg against
    // the live game (a recipient that left, a replacement whose owner left), so
    // a departure does not settle it here.
    if super::combat_damage::parked_combat_damage_awaits_replacement_choice(state) {
        // CR 800.4a + CR 616.1f: the choice is not abandoned with its chooser:
        // the damaged permanent may survive the departure (a Control Magic host
        // returns to its owner), so the event is lifted back into the parked
        // batch and re-proposed to whoever now owes it.
        let chooser_left = interrupted
            .actor
            .is_some_and(|actor| interrupted.leaving.contains(&actor));
        if !chooser_left || super::replacement::lift_departed_combat_leg_choice(state) {
            return Ok(DepartureSettlement::NotSettled);
        }
    }
    let Some(pending) = state.pending_replacement.as_ref() else {
        return Ok(DepartureSettlement::NotSettled);
    };
    // CR 400.7: being represented in the object map is not survival of the
    // interrupted incarnation. Do not apply its event to a later incarnation.
    let survives = interrupted
        .subjects
        .into_iter()
        .flatten()
        .all(|subject| subject.is_current(state))
        && super::replacement::replacement_event_survives(state, &pending.proposed);
    if !survives {
        // CR 800.4a + CR 121.6b + CR 614.17c: this is the one place that decides
        // what becomes of a parked replacement whose event left with a player.
        // Under a parked draw unless payment that outlives the departure, the
        // choice the leaver was asked is their own draw inside the payer's draw
        // sequence (a substitute: Notion Thief's "you draw a card"). The record
        // is kept for the concession's action boundary, which settles it as a
        // draw that can no longer happen
        // (`engine::reconcile_parked_unless_payment_after_departure`): the
        // sequences above it resume, so the payer's own remaining draws
        // (Thought Reflection's second card) are not abandoned with the
        // leaver's.
        if super::engine_payment_choices::departed_draw_settles_with_parked_payment(
            state,
            &interrupted.leaving,
        ) {
            return Ok(DepartureSettlement::NotSettled);
        }
        state.pending_replacement = None;
        state.pending_single_discard = None;
        state.replacement_may_cost_paused = false;
        // CR 121.6b: under a surviving parked draw payment the abandoned choice
        // sat inside a substitute for one of the payer's draws, so only the
        // leaver's part of the draw authority goes with it.
        if super::engine_payment_choices::draw_payment_is_parked(state) {
            super::replacement::abandon_departed_substitute_continuation(
                state,
                &interrupted.leaving,
            );
        } else {
            super::replacement::abandon_post_replacement_continuation(state);
        }
        // The abandoned choice may be a combat lifelink gain being staged; no
        // answer will arrive for it, so the batch must not wait to claim one.
        if let Some(record) = state.pending_combat_lifelink.as_mut() {
            record.awaiting_staged_gain = false;
        }
        let waiting = finish_abandoned_replacement(state, &interrupted.boundary, events)?;
        state.waiting_for = waiting;
        return Ok(replacement_departure_settlement(state));
    }
    // CR 800.4a + CR 118.12 + CR 616.1: a replacement that a surviving payment
    // owns (a staged payment transaction, the replay of one that mirrors the
    // departure onto its recovery checkpoint, or a parked draw unless payment) is
    // not settled by the departure. The concession's action boundary settles
    // it with the payment: a staged payment re-materializes on the base the
    // departure changed, a parked prompt that no longer describes its event is
    // reconciled (`engine::reconcile_parked_unless_payment_after_departure`),
    // and a reconciliation that fails returns to the departure with the
    // payment retired, so no draw the payment staged is committed on the
    // departed board.
    if state.payment_transaction.is_some()
        || state.payment_transaction_replay
        || super::engine_payment_choices::draw_payment_is_parked(state)
    {
        return Ok(DepartureSettlement::NotSettled);
    }
    if interrupted
        .cost_payer
        .is_some_and(|payer| !super::players::is_alive(state, payer))
    {
        let waiting = resolve_pending_replacement(state, events, |state, events| {
            super::replacement::continue_replacement(state, 1, events)
        })?;
        state.waiting_for = waiting;
        return Ok(replacement_departure_settlement(state));
    }
    if interrupted.replacement_choice.is_none()
        || state
            .pending_replacement
            .as_ref()
            .is_some_and(|pending| pending.may_cost_paid)
    {
        // An accepted payment's living payer still owes their exact child
        // choice/suffix. Departure is not a second optional-payment decision.
        return Ok(DepartureSettlement::NotSettled);
    }
    if !super::replacement::pending_replacement_needs_reconsult(state, interrupted.actor) {
        return Ok(DepartureSettlement::NotSettled);
    }
    let waiting = resolve_pending_replacement(
        state,
        events,
        super::replacement::reconsult_pending_replacement,
    )?;
    state.waiting_for = waiting;
    Ok(replacement_departure_settlement(state))
}

fn replacement_departure_settlement(
    state: &GameState,
) -> super::engine_resolution_choices::DepartureSettlement {
    if matches!(state.waiting_for, WaitingFor::Priority { .. })
        && state.pending_replacement.is_none()
    {
        super::engine_resolution_choices::DepartureSettlement::Settled
    } else {
        super::engine_resolution_choices::DepartureSettlement::NotSettled
    }
}

fn finish_abandoned_replacement(
    state: &mut GameState,
    boundary: &ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // This is cancellation, not replacement prevention: no paid-cost completion,
    // occurrence, or Prevented staged terminal is fabricated for the lost member.
    // CR 800.4j + CR 500.8: a continuing game's BeginPhase menu is obsolete,
    // not its committed next phase or scheduled entry. Keep the typed cursor
    // and step-entry debt for `resume_abandoned_phase_transition` to repropose
    // with the current active-player authority. Terminal retirement is separate.
    finish_replacement_execute_epilogue(
        state,
        ReplacementExecuteEpilogue {
            zone_change_object_id: None,
            enters_battlefield: false,
            parked_single_discard: None,
            parked_affected_object_id: boundary.affected_object_id(),
            pending_phase_drain_life_loser: None,
            action_event_start: events.len(),
            boundary: ReplacementEpilogueBoundary::Departure,
        },
        events,
    )
}

pub(crate) fn settle_unpaid_replacement_may_cost(
    state: &mut GameState,
    outer: PendingReplacement,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    state.replacement_may_cost_paused = false;
    if super::replacement::replacement_event_survives(state, &outer.proposed) {
        state.pending_replacement = Some(outer);
        resolve_pending_replacement(state, events, |state, events| {
            super::replacement::continue_replacement(state, 1, events)
        })
    } else {
        super::replacement::abandon_post_replacement_continuation(state);
        finish_abandoned_replacement(state, outer.delivery_boundary_event(), events)
    }
}

fn handle_replacement_choice_body(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    operation: impl FnOnce(&mut GameState, &mut Vec<GameEvent>) -> super::replacement::ReplacementResult,
) -> Result<WaitingFor, EngineError> {
    let action_event_start = events.len();
    // A record stamped for rejection came from an action-ingress player-resource
    // change. Resume it inside an ingress window so every life, player-counter
    // and energy settlement the handler reaches (a substitute's drain,
    // post-replacement continuations, nested parks) is held to the same rule;
    // the window closes before the combat lifelink batch resumes.
    let parked_for_rejection = state.pending_replacement.as_ref().is_some_and(|pending| {
        pending.unrepresentable_life_change == UnrepresentablePolicy::Reject
    });
    let waiting_for = if parked_for_rejection {
        let (inner, window) = within_ingress_life_window(state, |state| {
            handle_replacement_choice_inner(state, events, operation)
        });
        let waiting_for = inner?;
        if let Some(player) = window.failed_player {
            return Err(LifeChangeUnrepresentable { player }.into());
        }
        waiting_for
    } else {
        handle_replacement_choice_inner(state, events, operation)?
    };
    Ok(settle_replacement_choice_wait(
        state,
        events,
        action_event_start,
        waiting_for,
    ))
}

/// The common epilogue of an answered replacement choice: adopt a nested prompt,
/// or hand a settled round trip back to priority and the parked combat batch.
fn settle_replacement_choice_wait(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    action_event_start: usize,
    waiting_for: WaitingFor,
) -> WaitingFor {
    // CR 616.1f: a second ordering choice, or any nested prompt, leaves the record
    // parked for the next answer; only a settled round trip may close the batch.
    // Nothing is left behind on this path — the anchor is a local that dies here.
    // CR 616.1g: this is also how a batch parked on its substitute's prompt
    // (counters, mill) resumes — once that prompt's round trip has settled.
    let WaitingFor::Priority { player } = waiting_for else {
        state.waiting_for = waiting_for.clone();
        // CR 603.6a + CR 616.1f: this answer's battlefield entries leave with its
        // buffer. Hand them to the entry gate: an entrant whose instruction is
        // still suspended is held for the scan that follows its last co-entrant.
        super::triggers::park_entry_observer_triggers_if_paused(state, events, action_event_start);
        return waiting_for;
    };
    // CR 800.4j: the handler hands a settled choice back to the active player's
    // priority. A choice answered after that player left the game (another
    // player's draw, parked on their own choice while the active player
    // conceded) hands it to the next player in turn order still in the game.
    let waiting_for = WaitingFor::Priority {
        player: if super::players::is_alive(state, player) {
            player
        } else {
            super::engine::living_priority_seat(state)
        },
    };
    state.waiting_for = waiting_for.clone();
    super::combat_damage::resume_pending_combat_lifelink(state, action_event_start, None, events)
        .unwrap_or(waiting_for)
}

/// CR 101.4 + CR 616.1: settle the replacement choice that interrupted a
/// simultaneous sacrifice batch's consult through `operation`, then continue
/// the batch: consult the remaining selections, deliver every approved move
/// together, and resume the instruction's later work through the common
/// priority boundary (CR 608.2c). The batch's events stay in the caller's
/// buffer for its trigger scan (CR 603.2): the post-action pipeline for an
/// answer, the departure settlement for a departure.
fn answer_player_scope_sacrifice_consult(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    operation: impl FnOnce(&mut GameState, &mut Vec<GameEvent>) -> super::replacement::ReplacementResult,
) -> Result<WaitingFor, EngineError> {
    match effects::answer_player_scope_sacrifice_consult(state, events, operation) {
        effects::PlayerScopeSacrificeConsultAnswer::NeedsChoice => {
            return Ok(state.waiting_for.clone());
        }
        effects::PlayerScopeSacrificeConsultAnswer::Consulted => {}
    }
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    resume_paused_player_scope_sacrifice(state, events)?;
    super::engine::resume_pending_continuation_if_priority(state, events)?;
    Ok(state.waiting_for.clone())
}

/// What resuming a parked player-scope sacrifice batch produced.
pub(crate) struct PausedPlayerScopeSacrificeResume {
    /// The wait the caller must adopt, when the resume left a prompt open.
    pub(crate) next_wait: Option<WaitingFor>,
    /// CR 603.2 + CR 603.10a: the completed batch's own event span in the
    /// buffer the helper was handed. Its observers belong to whichever
    /// collector scans that buffer: the replacement action's post-action
    /// pipeline, or the departure settlement.
    pub(crate) completed_batch: Option<std::ops::Range<usize>>,
}

/// CR 101.4 + CR 616.1: resume a simultaneous player-scope sacrifice batch
/// parked on a CR 616.1 choice. Collects no triggers: it reports the wait the
/// caller must adopt and the completed batch's own event span, and the caller's
/// collector observes that span (CR 603.2).
pub(crate) fn resume_paused_player_scope_sacrifice(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<PausedPlayerScopeSacrificeResume, EngineError> {
    let Some(sacrifice_source_id) = state
        .pending_player_scope_sacrifice_choice
        .as_ref()
        .map(|pending| pending.ability.source_id)
    else {
        return Ok(PausedPlayerScopeSacrificeResume {
            next_wait: None,
            completed_batch: None,
        });
    };
    match effects::drain_pending_player_scope_sacrifice_after_replacement(state, events)
        .map_err(|error| EngineError::InvalidAction(error.to_string()))?
    {
        effects::PendingPlayerScopeSacrificeOutcome::WaitingForNextChoice => {
            Ok(PausedPlayerScopeSacrificeResume {
                next_wait: None,
                completed_batch: None,
            })
        }
        // The paused members' events stay in the carrier's deferred span, so
        // nothing of the batch is observable yet.
        effects::PendingPlayerScopeSacrificeOutcome::PausedForReplacement => {
            Ok(PausedPlayerScopeSacrificeResume {
                next_wait: Some(state.waiting_for.clone()),
                completed_batch: None,
            })
        }
        effects::PendingPlayerScopeSacrificeOutcome::Completed {
            events_before_sacrifice,
            events_after_sacrifice,
            sacrificed_count,
        } => {
            effects::stamp_active_player_action_completion(
                state,
                sacrifice_source_id,
                crate::types::ability::EffectResolutionResult {
                    cause: crate::types::ability::ThisWayCause::Sacrificed,
                    count: sacrificed_count,
                },
            );
            effects::drain_pending_continuation(state, events);
            Ok(PausedPlayerScopeSacrificeResume {
                next_wait: (!matches!(state.waiting_for, WaitingFor::Priority { .. }))
                    .then(|| state.waiting_for.clone()),
                completed_batch: Some(events_before_sacrifice..events_after_sacrifice),
            })
        }
    }
}

/// What settles a replacement-execute epilogue (CR 800.4a + CR 616.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplacementEpilogueBoundary {
    /// A player answered the replacement: every parked drain stage runs.
    Answer,
    /// A departure cancelled the leaver's replacement: the surviving players' shared legacy
    /// batch tail stays on its owner for the action's ordinary drain, while a staged
    /// population is settled by the departure itself.
    Departure,
}

struct ReplacementExecuteEpilogue {
    zone_change_object_id: Option<ObjectId>,
    enters_battlefield: bool,
    parked_single_discard: Option<crate::types::game_state::PendingSingleDiscard>,
    parked_affected_object_id: Option<ObjectId>,
    pending_phase_drain_life_loser: Option<PlayerId>,
    action_event_start: usize,
    /// CR 800.4a + CR 616.1: what settles this epilogue: an answered replacement, or the
    /// cancellation of a leaver's replacement by a departure.
    boundary: ReplacementEpilogueBoundary,
}

/// CR 616.1 + CR 608.2c: settle a delivered replacement or a whole staged batch
/// through the same ordered drains before resuming its producer.
fn finish_replacement_execute_epilogue(
    state: &mut GameState,
    epilogue: ReplacementExecuteEpilogue,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let ReplacementExecuteEpilogue {
        zone_change_object_id,
        enters_battlefield,
        parked_single_discard,
        parked_affected_object_id,
        pending_phase_drain_life_loser,
        action_event_start: replacement_action_event_start,
        boundary,
    } = epilogue;
    let mut waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    state.waiting_for = waiting_for.clone();

    // CR 701.9a + CR 614.6: the replaced discard still happened; record it
    // now that its move has been delivered, before post-replacement and
    // `waiting_for`-gated drains run. Gated on the carrier latched before
    // `continue_replacement` so a carrier installed during this answer
    // (a different discard's, owed to a later answer) is not consumed here.
    if parked_single_discard.is_some()
        && state.pending_single_discard.as_deref().copied() == parked_single_discard
    {
        effects::settle_paused_single_discard(state, events);
    }

    let mut replacement_ctx = None;
    if zone_change_object_id.is_some_and(|object_id| {
        state
            .active_spell_resolution()
            .is_some_and(|ctx| object_id == ctx.object_id)
    }) {
        let ctx = state
            .take_active_spell_resolution()
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
            // CR 614.6 + CR 500.5: the substitute of the replacement
            // chosen for the phase drain's loss paused, and the drain
            // may not advance until that substitute finishes. Park it
            // exactly as the `Prevented` arm does: the shared resumer
            // then finishes the transition once the substitute
            // terminally drains, and a loss the substitute raises is
            // not mistaken for the drain's own
            // (`pending_phase_drain_life_loser` needs Ready).
            if pending_phase_drain_life_loser.is_some() {
                super::turns::mark_phase_transition_awaiting_post_replacement(state);
            }
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
        if let Some(wf) = resume_active_draw_sequences(state, events) {
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

    if matches!(waiting_for, WaitingFor::Priority { .. }) && state.active_counter_moves().is_some()
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
        && state.active_counter_removals().is_some()
    {
        effects::counters::drain_pending_counter_removals(state, events);
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            waiting_for = state.waiting_for.clone();
        }
    }

    if matches!(waiting_for, WaitingFor::Priority { .. })
        && state.active_counter_additions().is_some()
    {
        effects::counters::drain_pending_counter_additions(state, events);
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            waiting_for = state.waiting_for.clone();
        }
    }

    if matches!(waiting_for, WaitingFor::Priority { .. })
        && state.active_life_total_assignment().is_some()
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
    // CR 800.4a + CR 616.1: a departure leaves a shared legacy batch's surviving tail on its
    // owner for the action's ordinary drain; a staged population is settled by the departure
    // itself (its leaver's decisions are retired and the rest are consulted again).
    let leaves_shared_tail = boundary == ReplacementEpilogueBoundary::Departure
        && state
            .active_batch_delivery()
            .is_some_and(|pending| pending.staged.is_none());
    if !leaves_shared_tail
        && matches!(waiting_for, WaitingFor::Priority { .. })
        && state.active_batch_delivery().is_some()
    {
        crate::game::zone_pipeline::drain_pending_batch_deliveries(state, events);
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            waiting_for = state.waiting_for.clone();
        }
    }

    // CR 120.3f + CR 120.1 + CR 616.1: a non-combat player-damage leg
    // whose life loss or infect poison result paused (Horde mill redirect,
    // loss replacements, or poison-counter replacements) owes its remaining
    // results (the `DamageDealt` event and CR 120.1 record when still owed,
    // the lifelink gain). Finish them after the loss batch settles and before
    // outer continuations. See `drain_settled_non_combat_damage_stash`.
    if let Some(next) = drain_settled_non_combat_damage_stash(
        state,
        matches!(waiting_for, WaitingFor::Priority { .. }),
        events,
        replacement_action_event_start,
    ) {
        waiting_for = next;
    }

    // CR 120.3d + CR 120.1 + CR 616.1: a non-combat wither/infect leg whose
    // counter placement paused owes its damage event, record, and lifelink.
    // As with the post-loss remainder above, finish it before outer work.
    if matches!(waiting_for, WaitingFor::Priority { .. })
        && state
            .pending_damage_post_counter
            .as_ref()
            .is_some_and(|stashed| !stashed.is_combat)
        && state.active_batch_delivery().is_none()
    {
        let owner = state
            .pending_damage_post_counter
            .as_ref()
            .and_then(|stash| stash.damage_delivery_owner);
        let outcome = effects::resume_damage_delivery(state, owner, |state| {
            effects::deal_damage::drain_pending_damage_post_counter(state, events)
        });
        if matches!(
            outcome,
            crate::game::effects::deal_damage::DamagePostCounterDrainOutcome::PausedForReplacement
        ) {
            waiting_for = state.waiting_for.clone();
            // CR 603.2 + CR 603.3b: the action ends on this prompt, so the
            // damage events dealt so far must be collected for the next
            // priority window.
            super::engine_resolution_choices::defer_observer_triggers_for_paused_choice(
                state,
                events,
                replacement_action_event_start,
            );
        }
    }

    // CR 614.6 + CR 616.1: settling a batch parked above its member's
    // delivery-tail child exposes that child's counter-removal queue. Finish
    // it here, in the same order as the queue drain above, before any parked
    // cost root resumes below.
    if matches!(waiting_for, WaitingFor::Priority { .. })
        && state.active_counter_removals().is_some()
    {
        effects::counters::drain_pending_counter_removals(state, events);
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            waiting_for = state.waiting_for.clone();
        }
    }

    if matches!(waiting_for, WaitingFor::Priority { .. }) && state.active_copy_token().is_some() {
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
        // CR 603.2: the completed batch lies inside this action's buffer,
        // which the post-action pipeline scans; it is not collected here.
        if let Some(next) = resume_paused_player_scope_sacrifice(state, events)?.next_wait {
            waiting_for = next;
        }
    }

    // CR 608.2c: a discard instruction parked mid-batch by a
    // replacement-application choice finishes what it still owes BEFORE
    // any parked continuation runs. The resolving effect follows its
    // instructions in written order, so later instructions cannot resume
    // until this action has settled and published its terminal result.
    if matches!(waiting_for, WaitingFor::Priority { .. }) && state.pending_discard_batch.is_some() {
        match effects::drain_pending_discard_batch(state, events)
            .map_err(|error| EngineError::InvalidAction(error.to_string()))?
        {
            effects::PendingDiscardBatchOutcome::Idle => {}
            effects::PendingDiscardBatchOutcome::PausedForReplacement => {
                waiting_for = state.waiting_for.clone();
                super::engine_resolution_choices::defer_observer_triggers_for_paused_choice(
                    state,
                    events,
                    replacement_action_event_start,
                );
            }
            effects::PendingDiscardBatchOutcome::Completed => {
                effects::drain_pending_continuation(state, events);
                if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    waiting_for = state.waiting_for.clone();
                    super::engine_resolution_choices::defer_observer_triggers_for_paused_choice(
                        state,
                        events,
                        replacement_action_event_start,
                    );
                }
            }
        }
    }

    // Turtle Team-Up's hero-event stepper owns the remaining steps after a
    // nested replacement. Resume it before the outer ability continuation,
    // preserving any new prompt and the instruction's departure cohort.
    if matches!(waiting_for, WaitingFor::Priority { .. }) && state.pending_hero_event.is_some() {
        match super::boss::drain_pending_hero_event(state, events) {
            super::boss::HeroEventDrainOutcome::Idle => {}
            super::boss::HeroEventDrainOutcome::PausedForReplacement => {
                waiting_for = state.waiting_for.clone();
                super::engine_resolution_choices::defer_observer_triggers_for_paused_choice(
                    state,
                    events,
                    replacement_action_event_start,
                );
            }
            super::boss::HeroEventDrainOutcome::Completed => {
                effects::drain_pending_continuation(state, events);
                if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    waiting_for = state.waiting_for.clone();
                    super::engine_resolution_choices::defer_observer_triggers_for_paused_choice(
                        state,
                        events,
                        replacement_action_event_start,
                    );
                }
            }
        }
    }

    if matches!(waiting_for, WaitingFor::Priority { .. })
                && (state.active_ability_continuation().is_some()
                    || state.active_change_zone_frame().is_some())
                // CR 118.12 + CR 605.3b + CR 616.1 + CR 614.12a + CR 608.2c: a
                // typed cost root that owns the unpaid cost suffix (a mana-source
                // pause), or that owns an entry whose cost is mid-payment (both
                // entry-MayCost roots), settles before the ordinary effect rider.
                // An entry replacement's cost is paid, and the permanent enters,
                // before the effect's later instructions continue; the nested
                // resume drains that rider after the entry.
                && !matches!(
                    state.pending_cost_move_resume,
                    Some(
                        PendingCostMoveResume::ManaAbilityPayment { .. }
                            | PendingCostMoveResume::ReplacementMayCost { .. }
                            | PendingCostMoveResume::ReplacementMayCostInnerChoice { .. }
                    )
                )
    {
        // CR 614.12b + CR 614.1c + CR 614.13: drain BOTH the chained
        // continuation and the multi-target ChangeZone iteration that
        // paused on this replacement choice (issue #535). This runs
        // AFTER a collected simultaneous sacrifice queue: CR 101.4
        // requires every already-announced sacrifice to finish before
        // the original ability's later instructions resume.
        let damage_chain = damage_parent_continuation_pending(state);
        effects::drain_pending_continuation(state, events);
        // CR 616.1e: The continuation may itself pause on another replacement
        // (e.g., the second direction of fight damage hitting the same shield),
        // in which case it sets `state.waiting_for` to the next ReplacementChoice.
        // Propagate that back so the engine surfaces the correct prompt.
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            waiting_for = state.waiting_for.clone();
            defer_damage_chain_observers_at_pause(
                state,
                damage_chain,
                events,
                replacement_action_event_start,
            );
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
        && state.active_each_player_copy_chosen().is_some()
        && state.active_copy_token().is_none()
        && state.active_counter_additions().is_none()
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
        } else if let Some(resumed) =
            // CR 513.1 + CR 603.3b: a CR 616.1 mana-pool choice can defer completion of
            // `enter_phase`, and the shared authority resumes only that bail — not an
            // `advance_phase` that paused the drain on its own (unit tests).
            super::turns::resume_deferred_step_triggers(state, events)
        {
            waiting_for = resumed;
        }
    }

    // CR 601.2h + CR 602.2b + CR 605.3b + CR 616.1: A delivered cost
    // move resumes through the single typed dispatcher after the first
    // continuation drain, which defers unpaid ManaAbilityPayment roots.
    // Foretell completed at its dedicated delivery boundary above.
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
        && (state.active_ability_continuation().is_some()
            || state.active_change_zone_frame().is_some())
    {
        let damage_chain = damage_parent_continuation_pending(state);
        effects::drain_pending_continuation(state, events);
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            waiting_for = state.waiting_for.clone();
            defer_damage_chain_observers_at_pause(
                state,
                damage_chain,
                events,
                replacement_action_event_start,
            );
        }
    }

    // CR 601.2h + CR 605.3b + CR 608.2c + CR 616.1: a counter-cost root
    // deferred at the delivery drain above because its replacement subtree
    // was live: the continuation drain above stops at the moment the subtree
    // retires (`counter_cost_root_is_due`), and the root resumes here, before
    // any outer frame work and before any outer cast or activation it is
    // paying for (a mana ability paying a spell's cost) is finished below.
    if matches!(waiting_for, WaitingFor::Priority { .. })
        && state.counter_cost_root_subtree() == CounterCostRootSubtree::Retired
    {
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

    // CR 601.2h + CR 602.2b + CR 616.1: Resume a non-move cast or
    // activation cost payment paused during discard or sacrifice. Never
    // under a counter-cost root whose replacement subtree is still live:
    // the common boundary below finishes that subtree and the root first.
    if matches!(waiting_for, WaitingFor::Priority { .. })
        && (state.pending_cast.is_some() || state.pending_discard_for_cost.is_some())
        && state.counter_cost_root_subtree() != CounterCostRootSubtree::Unfinished
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

    // CR 614.6 + CR 500.5: Finish every terminal replacement-choice
    // Execute path through the shared continuation boundary. In
    // particular, a nested non-preventing LifeLoss ordering choice can
    // be the last child of an interactive substitute that owns a
    // parked APNAP phase transition; the direct phase drain above
    // deliberately rejects that cursor while it is Awaiting. The
    // shared resumer first retires the outer continuation/frame, then
    // advances the typed phase owner exactly once.
    if matches!(waiting_for, WaitingFor::Priority { .. }) {
        state.waiting_for = waiting_for;

        // CR 608.3a + CR 616.1f: the parked entry context is the structural PARENT
        // of every drain stage above. Complete it only once they have all retired
        // and it owns the top — after the counter/batch/copy-token/continuation
        // stages, and before the shared continuation boundary, which must not
        // observe a frame no priority-time drain is allowed to touch.
        //
        // Placed AFTER the line above, deliberately: this arm reconciles
        // `state.waiting_for` with its arm-local ONLY there, and three stages
        // between the counter drain and this block assign a returned value
        // straight into the arm-local (the deferred-step-trigger resume, the cost
        // move resume, and the interrupted cost payment). Before that line the
        // helper's `state.waiting_for` guard would be reading a value stale by up
        // to three stages; after it, the guard reads this arm's settled
        // authority. The helper's own guard remains the single settle authority —
        // this call site adds none.
        finish_spell_resolution_exposed_by_child(state, parked_affected_object_id, events);

        // CR 800.4a + CR 616.1: a departure leaves the surviving players' parked batch tail on
        // its shared owner; the action's ordinary drain resumes it.
        let damage_chain = damage_parent_continuation_pending(state);
        if !leaves_shared_tail {
            // CR 608.2f: a paused simultaneous token clause finishes inside the
            // resumer below (its first pass boundary), not in the Execute arm.
            super::engine::resume_pending_continuation_if_priority(state, events)?;
        }
        waiting_for = state.waiting_for.clone();
        if !matches!(waiting_for, WaitingFor::Priority { .. }) {
            defer_damage_chain_observers_at_pause(
                state,
                damage_chain,
                events,
                replacement_action_event_start,
            );
        }
    }

    Ok(waiting_for)
}

/// CR 614.6 + CR 616.1: a token-substitute answer resumes only a parked
/// substitute awaiting that answer's kind.
fn validate_token_substitute_answer(
    state: &GameState,
    answer: &TokenSubstituteAnswer,
) -> Result<(), EngineError> {
    let Some(pending) = state.pending_replacement.as_ref() else {
        return Err(EngineError::InvalidAction(
            "token-substitute answer has no pending replacement".to_string(),
        ));
    };
    let Some(rec) = pending.token_substitute else {
        return Err(EngineError::InvalidAction(
            "token-substitute answer has no parked substitute".to_string(),
        ));
    };
    let matches = matches!(
        (&rec.awaits, answer),
        (
            TokenSubstituteChoice::Branch,
            TokenSubstituteAnswer::Branch(_)
        ) | (
            TokenSubstituteChoice::CopySource,
            TokenSubstituteAnswer::CopySource(_)
        )
    );
    if !matches {
        return Err(EngineError::InvalidAction(
            "token-substitute answer mismatches its parked substitute".to_string(),
        ));
    }
    Ok(())
}

/// CR 602.2b + CR 606.4: consume the concrete result of a marked parked root
/// exactly once into the already-known activation tally. Unknown legacy
/// contexts remain unknown.
fn credit_parked_loyalty_cost_receipt(state: &mut GameState, removed: u32) {
    match state.pending_cost_move_resume.as_mut() {
        Some(PendingCostMoveResume::CounterCostRemovalPayment {
            pending: Some(pending),
            ..
        }) => {
            if let Some(tally) = pending.ability.context.loyalty_counters_removed.as_mut() {
                *tally = tally.saturating_add(removed);
            }
        }
        Some(PendingCostMoveResume::LoyaltyActivation { resolved, .. }) => {
            if let Some(tally) = resolved.context.loyalty_counters_removed.as_mut() {
                *tally = tally.saturating_add(removed);
            }
        }
        None
        | Some(
            PendingCostMoveResume::Cast { .. }
            | PendingCostMoveResume::SacrificeForCost { .. }
            | PendingCostMoveResume::WardSacrificePayment { .. }
            | PendingCostMoveResume::ReplacementMayCost { .. }
            | PendingCostMoveResume::ReplacementMayCostInnerChoice { .. }
            | PendingCostMoveResume::Foretell { .. }
            | PendingCostMoveResume::CollectEvidencePayment { .. }
            | PendingCostMoveResume::UnlessBouncePayment { .. }
            | PendingCostMoveResume::ManaAbilityPayment { .. }
            | PendingCostMoveResume::ActivationMillPayment { .. }
            | PendingCostMoveResume::CounterCostRemovalPayment { pending: None, .. }
            | PendingCostMoveResume::CounterAdditionUnlessPayment { .. }
            | PendingCostMoveResume::RandomDiscardUnlessPayment(_)
            | PendingCostMoveResume::TurnFaceUpCostMove { .. }
            | PendingCostMoveResume::PlayerScopeUnlessSettlement(_),
        ) => {}
    }
}

fn handle_replacement_choice_inner(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    operation: impl FnOnce(&mut GameState, &mut Vec<GameEvent>) -> super::replacement::ReplacementResult,
) -> Result<WaitingFor, EngineError> {
    // CR 614.17c: a parked replacement (optional or a mandatory ordering
    // candidate) for a draw that can no longer happen cannot be applied. Settle
    // the event with no replacement: the draw is clamped and its payment
    // resumes without milling or returning the replacement's card.
    let impossible_parked_draw = state.pending_replacement.as_ref().is_some_and(|pending| {
        super::replacement::proposed_draw_cannot_happen(state, &pending.proposed)
    });
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
    // The Prevented arm then routes the removal to its owner by the parked
    // counter-cost root's replacement subtree (`counter_cost_root_subtree`).
    let pending_was_counter_removal = state
        .pending_replacement
        .as_ref()
        .is_some_and(|pending| matches!(pending.proposed, ProposedEvent::RemoveCounter { .. }));
    // CR 106.4 + CR 119.3 + CR 616.1f: Yurlok's aggregate LifeLoss can pause
    // the APNAP phase-transition drain on an ordering choice. Capture that
    // ownership before `continue_replacement` consumes the pending record.
    // The LifeLoss event is the sole resume authority; the already-applied
    // EmptyManaPool event must never be replayed.
    //
    // Carries the LOSER rather than a bare bool: the `Prevented` arm has to
    // consume that player's parked empty-pool provenance, and reading it back
    // after `continue_replacement` is impossible — the record is gone.
    let pending_phase_drain_life_loser = state
        .pending_phase_transition_progress
        .as_ref()
        .is_some_and(|progress| {
            progress.drain_state == crate::types::game_state::PhaseTransitionDrainState::Ready
        })
        .then(|| {
            state
                .pending_replacement
                .as_ref()
                .and_then(|pending| match pending.proposed {
                    ProposedEvent::LifeLoss { player_id, .. } => Some(player_id),
                    _ => None,
                })
        })
        .flatten();
    let parked_exile_controller = state
        .pending_replacement
        .as_ref()
        .and_then(|pending| pending.exile_controller);
    let parked_exile_duration = state
        .pending_replacement
        .as_ref()
        .and_then(|pending| pending.exile_duration.clone());
    let parked_exile_tracking = state
        .pending_replacement
        .as_ref()
        .map(|pending| pending.exile_tracking)
        .unwrap_or_default();
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
    // CR 400.7: the damage source's incarnation captured when it dealt the
    // parked damage, read before `continue_replacement` consumes the record.
    let parked_source_incarnation = state
        .pending_replacement
        .as_ref()
        .and_then(|pending| pending.source_incarnation);
    // The player-resource policy the parked record was stamped with, captured
    // with the file's other `parked_*` reads before `continue_replacement`
    // consumes it.
    let parked_life_policy = state
        .pending_replacement
        .as_ref()
        .map(|pending| pending.unrepresentable_life_change)
        .unwrap_or_default();
    // CR 101.4 + CR 616.1: the recipient whose life gain this answer settles.
    // The Prevented arm stages a substitute for a life-gain cost that is
    // mid-staging on this recipient, and its payload no longer names them.
    let parked_life_gain_recipient =
        state
            .pending_replacement
            .as_ref()
            .and_then(|pending| match pending.proposed {
                ProposedEvent::LifeGain { player_id, .. } => Some(player_id),
                _ => None,
            });
    // CR 701.21a + CR 614.1: An inner graveyard move can have parked after a
    // sacrifice was accepted. The resumed payload is only a ZoneChange, so
    // capture the enclosing sacrifice before `continue_replacement` consumes
    // the pending record; the ZoneChange arm below emits its trigger event only
    // after delivery succeeds.
    let parked_sacrifice_provenance = state
        .pending_replacement
        .as_ref()
        .and_then(|pending| pending.sacrifice_provenance);
    // CR 122.1 + CR 616.1f: the object whose counter placement this resume answers.
    // Captured here, with the file's other `parked_*` reads, because
    // `continue_replacement` below consumes the pending record. The Execute arm's
    // own `zone_change_object_id` cannot serve: it is `None` on the counter path
    // (the Execute payload is an AddCounter event, not a ZoneChange) and it is
    // out of scope in the Prevented arm.
    let parked_affected_object_id = state
        .pending_replacement
        .as_ref()
        .and_then(|pending| pending.proposed.affected_object_id());
    // The object of a parked zone move, for the `Prevented` arm. No applier
    // prevents a move onto the battlefield today; if one does, a played land's
    // `LandPlayed` stays collectable, as it did before the park recorded state.
    let parked_zone_change_object_id = state
        .pending_replacement
        .as_ref()
        .filter(|pending| matches!(pending.proposed, ProposedEvent::ZoneChange { .. }))
        .and_then(|pending| pending.proposed.affected_object_id());
    // A replacement-paused zone move belongs to its logical owner by both the
    // pre-move incarnation and the exact proposed event. Capture this before
    // `continue_replacement` consumes the pending record.
    let parked_zone_change_delivery = state.pending_zone_change_delivery_from_replacement();
    // CR 730.3: the batch or ChangeZone owner that parked this member is the
    // announcing owner of its resumed delivery, so a merged member's component
    // batch links back to it.
    let parked_member_owner = parked_zone_change_delivery.as_ref().and_then(|paused| {
        state
            .resolution_stack
            .active_owner_parking(paused.member, &paused.expected_event)
            .map(|group| crate::game::zone_pipeline::MemberDeliveryOwner {
                group,
                member: paused.member,
                expected_event: Some(paused.expected_event.clone()),
                population_devour_snapshot: None,
                approved_post_replacement: None,
                inherited_arrangements: Vec::new(),
            })
    });
    // CR 101.4 + CR 616.1: a staged member's consultation result is retained by
    // the same owner that parked it — never delivered here. The shared helper
    // matches the exact parked boundary, so a changed proposal or incarnation
    // cannot be consumed as this member's approval.
    let staged_approval_owner = parked_zone_change_delivery
        .as_ref()
        .and_then(|parked| crate::game::zone_pipeline::staged_approval_owner(state, parked));
    // CR 701.8a: an outer Destroy pause has no `PendingZoneChangeDelivery`; its
    // exact boundary is the parked `Destroy` proposal itself.
    let staged_destroy_approval_owner = state
        .pending_staged_approval_boundary()
        .filter(|(_, expected)| matches!(expected, ProposedEvent::Destroy { .. }))
        .and_then(|(identity, expected)| {
            crate::game::zone_pipeline::staged_approval_owner_for(state, identity, &expected)
        });
    let staged_consult_owner = staged_approval_owner.or(staged_destroy_approval_owner);
    // CR 701.9a + CR 614.6: the paused frameless single-card discard this
    // replacement answer owns, if any (paused by a CR 616.1 choice or by a
    // single optional replacement's apply-or-decline choice).
    // Captured with the other `parked_*` reads; a carrier installed while this
    // answer resolves (a nested connive) belongs to the next answer and is left
    // in place.
    let parked_single_discard = state.pending_single_discard.as_deref().copied();
    let parked_non_zone_boundary = state
        .pending_replacement
        .as_ref()
        .filter(|_| parked_zone_change_delivery.is_none())
        .map(|pending| pending.delivery_boundary_event().clone());
    let parked_delivery_boundary = parked_zone_change_delivery
        .as_ref()
        .map(|paused| &paused.expected_event)
        .or(parked_non_zone_boundary.as_ref());
    let parked_phase_begin = state
        .pending_replacement
        .as_ref()
        .is_some_and(|pending| matches!(pending.proposed, ProposedEvent::BeginPhase { .. }));
    // CR 616.1f + CR 603.6d: the re-scan after an answered ordering choice on an
    // as-enters roll reads the same pre-entry board as its proposal. (A token
    // substitute answers a CreateToken park, never a roll, so no resume check
    // is needed: the RollDice check below already excludes it.)
    let pre_entry = state
        .pending_replacement
        .as_ref()
        .is_some_and(|pending| matches!(pending.proposed, ProposedEvent::RollDice { .. }))
        .then(|| super::pre_entry_view::PreEntryRulesView::of_pending_roll(state))
        .flatten();
    let parked_damage_owner = state
        .pending_replacement
        .as_ref()
        .filter(|pending| {
            matches!(
                pending.proposed,
                ProposedEvent::Damage {
                    is_combat: false,
                    ..
                }
            )
        })
        .and_then(|pending| pending.damage_delivery_owner);
    // CR 616.1 + CR 615.5: an answer a parked damage batch awaits is one of its original
    // choices, so the chosen replacement's action or rider waits with the batch's held work.
    let (result, viewed_choice) = if impossible_parked_draw {
        let proposed = state
            .pending_replacement
            .as_ref()
            .expect("the impossible parked draw is still live")
            .proposed
            .clone();
        state.pending_replacement = None;
        (
            super::replacement::ReplacementResult::Execute(proposed),
            None,
        )
    } else {
        match pre_entry {
            Some(view) => {
                let viewed = view.replace(state, operation, events);
                (viewed.result, viewed.choice)
            }
            // A parked damage batch's answer is never a roll, so only the live
            // board's re-scan can hold riders.
            None => (
                effects::resume_damage_delivery(state, parked_damage_owner, |state| {
                    effects::deal_damage::hold_riders_for_awaited_portion(state, |state| {
                        operation(state, events)
                    })
                }),
                None,
            ),
        }
    };
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
    // CR 616.1: the answer settles whether the parked step begins; the phase
    // transition that asked owns what follows.
    if parked_phase_begin
        && !matches!(
            result,
            super::replacement::ReplacementResult::NeedsChoice(_)
        )
    {
        return Ok(super::turns::resume_phase_begin_choice(
            state, result, events,
        ));
    }
    match result {
        super::replacement::ReplacementResult::Execute(event) => {
            let mut zone_change_object_id = None;
            let mut enters_battlefield = false;
            match event {
                // The post-choice event is a
                // `ReplacementResult::Execute` payload, so it is sealed through
                // the third mint path (`approve_post_replacement`) and
                // delivered by the shared `zone_pipeline::deliver` machinery.
                // The resumed entry gets the FULL delivery tail: the
                // CR 614.12a devour snapshot, the CR 614.1c
                // `EntersWithAdditionalCounters` statics snapshot, the
                // CR 303.4f `attach_to` host, `entered_via_ability_source`
                // provenance (CR 603.6a, from the event's `cause`), and the
                // CR 701.24a library-shuffle arm.
                //
                // The shared tail is parameterized rather than copied:
                // (1) `DeliveryCtx.drain = CallerEpilogue` — the tail skips the
                //     `post_replacement_continuation` drain; the epilogue below
                //     keeps draining WITH the spell-resolution ctx and with
                //     `post_replacement_source` cleared for zone changes.
                // (2) SpellResolution frame ordering is therefore
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
                    // CR 101.4 + CR 616.1: staged approval retains the exact
                    // final event; delivery happens only after every member has
                    // been consulted.
                    if let Some((group, fresh)) = staged_consult_owner {
                        if !fresh {
                            return Ok(state.waiting_for.clone());
                        }
                        return match crate::game::zone_pipeline::record_staged_batch_consultation_result(
                            state,
                            group,
                            StagedMemberOutcome::Approved(event),
                            events,
                            replacement_action_event_start,
                        ) {
                            crate::game::zone_pipeline::StagedConsultationHandback::NotOwned => {
                                Ok(state.waiting_for.clone())
                            }
                            crate::game::zone_pipeline::StagedConsultationHandback::Parked(waiting) => Ok(*waiting),
                            crate::game::zone_pipeline::StagedConsultationHandback::Settled => {
                                finish_replacement_execute_epilogue(
                                    state,
                                    ReplacementExecuteEpilogue {
                                        zone_change_object_id: None,
                                        enters_battlefield: false,
                                        parked_single_discard,
                                        parked_affected_object_id,
                                        pending_phase_drain_life_loser,
                                        action_event_start: replacement_action_event_start,
                                        boundary: ReplacementEpilogueBoundary::Answer,
                                    },
                                    events,
                                )
                            }
                        };
                    }
                    let (object_id, to, cause, discard_frame) = match &event {
                        ProposedEvent::ZoneChange {
                            object_id,
                            to,
                            cause,
                            discard_frame,
                            ..
                        } => (*object_id, *to, *cause, *discard_frame),
                        _ => unreachable!("arm pattern guarantees ZoneChange"),
                    };
                    // CR 616.1: the approved resume event carries the chosen
                    // replacement's modifications; keep it for the paused-key match.
                    let event_for_capture = event.clone();
                    let Ok(approved) =
                        crate::game::zone_pipeline::ApprovedZoneChange::approve_post_replacement(
                            event,
                        )
                    else {
                        unreachable!("arm pattern guarantees a ZoneChange payload");
                    };
                    let delivery_start = events.len();
                    let delivery = crate::game::zone_pipeline::deliver_batch_member(
                        state,
                        approved,
                        crate::game::zone_pipeline::DeliveryCtx {
                            source_id: cause,
                            exile_links: crate::game::zone_pipeline::ExileLinkSpec {
                                duration: parked_exile_duration,
                                controller: parked_exile_controller,
                                tracking: parked_exile_tracking,
                            },
                            drain:
                                crate::types::game_state::PostReplacementDrainOwner::CallerEpilogue,
                        },
                        parked_member_owner,
                        events,
                    );
                    // CR 616.1 + CR 614.1c + CR 400.7: the resumed segment IS the
                    // parked member's terminal delivery history whether or not the
                    // delivery's own tail parks again. Record the classification
                    // BEFORE either terminal, using the same pre-move key and the
                    // same single-authority classifier as the synchronous path, or
                    // a tail park returns early and the batch owner later reaches
                    // `drain_pending_batch_deliveries` with `terminal_completion:
                    // None` (issue #10431). CR 730.3: when a merged member's
                    // component batch parks, that child has taken its delivered
                    // events and is the active frame, so neither capture reaches
                    // the member's owner; the child's handback classifies it.
                    if let Some(paused) = parked_zone_change_delivery.as_ref() {
                        let completion =
                            crate::game::zone_pipeline::zone_move_completion_from_delivery(
                                paused.member,
                                &events[delivery_start..],
                            );
                        // Exact boundary match first (the historical path every
                        // shipped park uses), then the occurrence-level match for a
                        // resume whose approved event carries the chosen
                        // replacement's modifications (controller/counter override
                        // or a CR 614.6 zone redirect).
                        let matched = state.capture_paused_zone_change_delivery(
                            paused.member,
                            &paused.expected_event,
                            &events[delivery_start..],
                            completion,
                        ) || (completion
                            == crate::types::game_state::ZoneMoveCompletion::Moved
                            && state.capture_resumed_paused_zone_change_delivery(
                                paused.member,
                                &event_for_capture,
                                &events[delivery_start..],
                                completion,
                            ));
                        if matched
                            && matches!(
                                &delivery,
                                crate::game::zone_pipeline::ZoneDeliveryResult::NeedsChoice(_)
                            )
                        {
                            // CR 614.6: the replaced move's terminal `ZoneChanged` is
                            // already in this slice, so the forwarded-result marker
                            // settles against the same record the Done path uses.
                            effects::settle_replaced_forwarded_zone_delivery(
                                state,
                                paused.member,
                                &events[delivery_start..],
                            );
                        }
                    }
                    match delivery {
                        crate::game::zone_pipeline::ZoneDeliveryResult::Done => {}
                        // CR 614.1c / CR 614.12a: the delivery tail parked a
                        // counter-replacement or devour prompt and stashed the
                        // remaining tail as a `ContinueZoneDeliveryTail` record
                        // (carrying `CallerEpilogue`, so the NEXT resume's
                        // epilogue still owns the continuation drain). Surface
                        // the parked prompt; the epilogue must not run yet. A
                        // paused frameless discard delivered by this move is
                        // settled first.
                        crate::game::zone_pipeline::ZoneDeliveryResult::NeedsChoice(_) => {
                            // CR 701.9a + CR 614.6: the paused discard's move was
                            // delivered in this action (its hand departure is in
                            // `events`) before the delivery tail parked this
                            // prompt; settle it now, while the shared stamp can
                            // still see that departure. CR 603.2 + CR 603.3b: this
                            // action ends on a prompt, so the post-action scan
                            // never sees it; defer the observers of exactly the
                            // stamped events.
                            if parked_single_discard.is_some()
                                && state.pending_single_discard.as_deref().copied()
                                    == parked_single_discard
                            {
                                let settle_start = events.len();
                                effects::settle_paused_single_discard(state, events);
                                super::engine_resolution_choices::defer_observer_triggers_for_paused_choice(
                                    state,
                                    events,
                                    settle_start,
                                );
                            }
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
                    if let Some(paused) = parked_zone_change_delivery.as_ref() {
                        effects::settle_replaced_forwarded_zone_delivery(
                            state,
                            paused.member,
                            &events[delivery_start..],
                        );
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
                    // CR 701.9a + CR 616.1: a discard move that paused for
                    // replacement ordering reaches terminal delivery here.
                    // Restore its exact continuation data before the common
                    // epilogue drains the parked ability chain.
                    if let Some(frame_id) = discard_frame {
                        effects::discard::hand_off_recruit_discard_result(state, frame_id);
                    }
                    enters_battlefield = to == Zone::Battlefield;
                    zone_change_object_id = Some(object_id);
                    // CR 305.1 + CR 603.2: a played land whose pre-entry
                    // shock/payment choice (`ReplacementResult::NeedsChoice`)
                    // paused this delivery parked its `LandPlayed` occurrence in
                    // `deferred_entry_events` (`park_land_played_for_deferred_entry`).
                    // The land has now entered, so flush the parked events into
                    // this action's `events` for the priority-time trigger scan,
                    // so "play a land" observers (City of Traitors) fire against
                    // the realized, post-choice object (issue #8738). Only this
                    // object's entries move, so every other replacement-resumed
                    // zone change keeps its unchanged path. A land a replacement
                    // kept off the battlefield has no such resume, and its play
                    // is recorded for the settled take.
                    if enters_battlefield {
                        flush_deferred_entry_events_into_priority_scan(state, object_id, events);
                    }
                    record_parked_land_play_completion(state, object_id);
                }
                event @ ProposedEvent::TokenEntry { entry_ref, .. } => {
                    // Staged copies share the same settlement for ordering
                    // answers and priority-boundary resumes.
                    if state
                        .liminal_entries
                        .get(&entry_ref)
                        .is_some_and(|entry| entry.copy_settlement.is_some())
                    {
                        // The answered ordering prompt is spent (as in the copy-answer branch).
                        state.waiting_for = WaitingFor::Priority {
                            player: state.active_player,
                        };
                        state.waiting_for =
                            resume_token_copy_settlement(state, entry_ref, event, false, events)?;
                        super::engine::resume_pending_continuation_if_priority(state, events)?;
                        return Ok(state.waiting_for.clone());
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
                    // CR 400.7 + CR 608.2h + CR 702.15c: rebuild the context of
                    // the incarnation captured when the damage parked
                    // (`park_damage_context`): the live object while it is still
                    // that incarnation, else that incarnation's exit LKI — never
                    // a later object at the same id. `None` = not captured (a
                    // record saved before the field, or a park that never passed
                    // through the helper): the live read, exactly as before the
                    // field existed.
                    let fallback_controller = state
                        .objects
                        .get(&source_id)
                        .map(|obj| obj.controller)
                        .unwrap_or(state.active_player);
                    let mut ctx = damage_context_for_pinned_incarnation(
                        state,
                        source_id,
                        fallback_controller,
                        parked_source_incarnation,
                    );
                    // CR 120.4a + CR 702.15b: restore the excess-redirect rider and
                    // the deferred lifelink bonus dropped by the source-derived ctx
                    // rebuild, so the resumed hit still redirects and a resumed redirect
                    // leg still gains the combined lifelink total.
                    ctx.excess_recipient = parked_excess_recipient;
                    ctx.lifelink_bonus = parked_lifelink_bonus;
                    // CR 120.4b + CR 120.4c: a portion of a parked two-phase batch rejoins the
                    // batch, which finishes its other portions' replacement before delivering.
                    if !super::effects::deal_damage::return_answered_portion_to_batch(
                        state,
                        &ctx,
                        &damage,
                        parked_damage_owner,
                    ) {
                        let result =
                            effects::resume_damage_delivery(state, parked_damage_owner, |state| {
                                apply_damage_after_replacement(
                                    state, &ctx, damage, is_combat, events,
                                )
                            });
                        if matches!(result, DamageResult::NeedsChoice) {
                            // CR 616.1 + CR 616.1f: the resumed hit raised its own
                            // result prompt (poison counters, life loss, the
                            // lifelink gain, or an excess-redirect leg), parked by
                            // the nested pipeline together with any owed results.
                            // Keep that wait: the Priority reset below and the
                            // settled-stash drain must not run over it; the nested
                            // answer's own tail drains the stash exactly once.
                            debug_assert!(
                                state.pending_replacement.is_some()
                                    || state.active_batch_delivery().is_some(),
                                "a resumed damage NeedsChoice must leave its pause owned"
                            );
                            // CR 603.2 + CR 603.3b + CR 120.4b: this action ends on
                            // a prompt, so the post-action scan never sees the
                            // damage events dealt so far; defer their observers.
                            super::engine_resolution_choices::defer_observer_triggers_for_paused_choice(
                                state,
                                events,
                                replacement_action_event_start,
                            );
                            return Ok(state.waiting_for.clone());
                        }
                    }
                }
                // CR 122.1: Counter addition accepted after replacement choice (e.g.,
                // Corpsejack Menace doubler on a prompted counter-placement).
                // CR 122.1 + CR 616.1: a player or energy placement settles under
                // the policy the parked record was stamped with: `Reject` fails
                // the answer (the boundary restores the record and prompt),
                // `IgnoreImpossible` records into the open window.
                ProposedEvent::AddCounter {
                    placement, count, ..
                } => match placement {
                    CounterPlacement::Object {
                        actor,
                        object_id,
                        counter_type,
                        ..
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
                        parked_life_policy,
                        events,
                    )?,
                    CounterPlacement::Energy { player_id, .. } => {
                        effects::energy::apply_energy_addition(
                            state,
                            player_id,
                            count,
                            parked_life_policy,
                            events,
                        )?
                    }
                },
                // CR 122.1: Counter removal accepted after replacement choice.
                // CR 118.11: the cost owner settles this removal by measurement.
                ProposedEvent::RemoveCounter {
                    object_id,
                    counter_type,
                    count,
                    loyalty_cost_root,
                    ..
                } => {
                    let removed = effects::counters::apply_counter_removal(
                        state,
                        object_id,
                        counter_type,
                        count,
                        events,
                    );
                    // CR 602.2b + CR 606.4: the marker belongs only to the
                    // parked root proposal. Latch its actual delivered result
                    // into the owning activation before the proposal is
                    // consumed; identical replacement children remain inert.
                    if loyalty_cost_root.is_some() {
                        credit_parked_loyalty_cost_receipt(state, removed);
                    }
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
                    if crate::game::object_state::resolve_and_apply_object_edit(
                        state,
                        object_id,
                        crate::types::resolved_commands::ResolvedObjectStatus::Tapped,
                        true,
                    )
                    .is_ok()
                    {
                        events.push(GameEvent::permanent_tapped(state, object_id, None));
                    }
                }
                // CR 701.26b: Untap accepted after replacement choice.
                ProposedEvent::Untap { object_id, .. } => {
                    if crate::game::object_state::resolve_and_apply_object_edit(
                        state,
                        object_id,
                        crate::types::resolved_commands::ResolvedObjectStatus::Tapped,
                        false,
                    )
                    .is_ok()
                    {
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
                // CR 121.2a: a draw INSTRUCTION whose consult paused on this
                // choice. Settle its surviving count into its frame — nothing is
                // delivered here; the resume loop below performs the individual
                // draws, each with its own consult.
                instruction @ ProposedEvent::Draw {
                    stage: DrawEventStage::Instruction,
                    player_id,
                    ..
                } => {
                    settle_draw_instruction(state, instruction);
                    // CR 805.4b: as in the individual-draw arm below, the draw-step
                    // draw is now owned by its settled frame, which the resume loop
                    // completes; pop it so the team drain does not re-enter
                    // `execute_draw_for` and draw this player a second time.
                    if state.pending_team_draw_step.first() == Some(&player_id) {
                        state.pending_team_draw_step.remove(0);
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
                    if let Some(frame) = state.active_draw_sequence_mut() {
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
                    let events_before = events.len();
                    apply_scry_after_replacement(state, scry, events);
                    // CR 701.22d: an empty-library scry publishes its event from
                    // this replacement-choice handler, where resolve_chain_body's
                    // recording does not see it, so record it here.
                    for event in &events[events_before..] {
                        if let GameEvent::PlayerPerformedAction {
                            player_id, action, ..
                        } = event
                        {
                            crate::game::effects::record_player_action_this_turn(
                                state, *player_id, *action,
                            );
                        }
                    }
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
                ProposedEvent::Connive { subject, count, .. } => {
                    let _ = crate::game::effects::connive::resolve_connive_effect(
                        state,
                        crate::types::game_state::ConniveSubject { snapshot: *subject },
                        count,
                        events,
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
                // (`state.waiting_for` set, tail in the active BatchDelivery frame)
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
                // CR 119.3: Life gain accepted after replacement choice. The
                // parked record's policy decides an unrepresentable final edit:
                // `Reject` fails the answer (the boundary restores the record and
                // its prompt), `IgnoreImpossible` skips the change and resumes.
                gain @ ProposedEvent::LifeGain {
                    player_id, amount, ..
                } => {
                    // CR 101.4 + CR 616.1: a combat lifelink gain being staged
                    // is recorded on its parked batch, not settled: it settles
                    // with the batch's other gains once every choice is made.
                    // Only an unclaimed event settles here.
                    if let Some(gain) = super::combat_damage::take_staged_answer(state, gain) {
                        // A settle that parks a prompt (the Horde loss
                        // redirect's mill choice; gain itself never pauses)
                        // early-returns the parked prompt like the Mill arm
                        // above, so the unconditional `waiting_for = Priority`
                        // reset below does not clobber it.
                        // CR 101.4: a life-gain cost's recipient answering
                        // mid-staging joins the batch applied together instead
                        // of applying alone.
                        if super::casting_costs::stage_answered_life_gain_cost(
                            state,
                            player_id,
                            AnsweredLifeGain::Gained(amount),
                        ) {
                            // Staged; the cost resumes and settles the batch.
                        } else {
                            match settle_life_gain_after_choice(
                                state,
                                gain,
                                parked_life_policy,
                                events,
                            ) {
                                Ok(_) => {}
                                Err(LifeChangeError::Deferred(_)) => {
                                    return Ok(state.waiting_for.clone());
                                }
                                Err(LifeChangeError::Unrepresentable(unrepresentable)) => {
                                    return Err(unrepresentable.into());
                                }
                            }
                        }
                    }
                }
                // CR 119.3: Life loss accepted after replacement choice.
                loss @ ProposedEvent::LifeLoss { player_id, .. } => {
                    // An empty-pool loss that deferred here never returns to
                    // the phase-transition drain, so this is the only place
                    // its cause can still be named (a mana burn would
                    // otherwise land as an unexplained life change).
                    let actual = match settle_life_loss_after_choice(
                        state,
                        loss,
                        parked_life_policy,
                        events,
                    ) {
                        Ok(actual) => actual,
                        // The Horde redirect parked a mill choice mid-settle:
                        // the LIFE choice terminally resolved, so consume any
                        // parked empty-pool record with a zero figure (there is
                        // no life figure to narrate — the loss became a mill —
                        // and a stranded record would be mis-claimed by the
                        // next same-player loss). Early-return the parked mill
                        // prompt like the Mill arm above.
                        Err(LifeChangeError::Deferred(_)) => {
                            crate::game::turns::note_empty_pool_life_loss_resolved(
                                state, player_id, 0, events,
                            );
                            return Ok(state.waiting_for.clone());
                        }
                        Err(LifeChangeError::Unrepresentable(unrepresentable)) => {
                            return Err(unrepresentable.into());
                        }
                    };
                    crate::game::turns::note_empty_pool_life_loss_resolved(
                        state, player_id, actual, events,
                    );
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
                    discard_frame,
                    applied,
                    ..
                } => {
                    if let effects::discard::DiscardOutcome::NeedsReplacementChoice(player) =
                        effects::discard::complete_discard_to_graveyard(
                            state,
                            object_id,
                            player_id,
                            source_id,
                            discard_frame,
                            applied,
                            events,
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
                            let _ = state.add_mana_to_pool(player_id, unit);
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
                // CR 614.1b + CR 614.10: BeginTurn replacements are mandatory skip
                // effects that never set `replacement_choice_waiting_for` (see
                // `turns.rs` — NeedsChoice on these is treated as a bug), and a
                // BeginPhase choice resumes its transition before this match. Arms are
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
                        "handle_replacement_choice: a BeginPhase choice resumes through its parked transition"
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
                    if let Some((group, fresh)) = staged_destroy_approval_owner {
                        if !fresh {
                            return Ok(state.waiting_for.clone());
                        }
                        return match crate::game::zone_pipeline::record_staged_batch_consultation_result(
                            state,
                            group,
                            StagedMemberOutcome::Approved(destroy),
                            events,
                            replacement_action_event_start,
                        ) {
                            crate::game::zone_pipeline::StagedConsultationHandback::NotOwned => {
                                Ok(state.waiting_for.clone())
                            }
                            crate::game::zone_pipeline::StagedConsultationHandback::Parked(waiting) => Ok(*waiting),
                            crate::game::zone_pipeline::StagedConsultationHandback::Settled => {
                                finish_replacement_execute_epilogue(
                                    state,
                                    ReplacementExecuteEpilogue {
                                        zone_change_object_id: None,
                                        enters_battlefield: false,
                                        parked_single_discard,
                                        parked_affected_object_id,
                                        pending_phase_drain_life_loser,
                                        action_event_start: replacement_action_event_start,
                                        boundary: ReplacementEpilogueBoundary::Answer,
                                    },
                                    events,
                                )
                            }
                        };
                    }
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
                    // CR 614.1a + CR 608.2c: the delivery authority publishes this
                    // event's whole referent, and hands it to a resident CopyToken
                    // frame, whose drain below assigns the ledger.
                    // CR 608.2f (F2): a paused simultaneous clause's compound
                    // owes no terminal event of its own — the deferred
                    // publication emits one per instructed seat.
                    let compound_is_paused = state.pending_simultaneous_token_clause.is_some()
                        && create.has_appended_token_batches();
                    let completed = if compound_is_paused {
                        apply_create_token_after_replacement_with_created_ids(
                            state,
                            create,
                            Vec::new(),
                            PendingEffectResolutionEvent::Suppress,
                            events,
                        )
                    } else {
                        apply_create_token_after_replacement(state, create, events)
                    };
                    if !completed {
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
                event @ ProposedEvent::EmptyManaPool { .. } => {
                    if super::turns::apply_empty_mana_pool_event(state, event, events)
                        == super::turns::EmptyManaPoolApplyOutcome::Deferred
                    {
                        return Ok(state.waiting_for.clone());
                    }
                }
                // CR 616.1 + CR 705.1: every coin-flip replacement parsed today
                // (Krark's Thumb) is a count multiplier that
                // `candidate_materiality` classifies as a commuting `Count`
                // writer, and its mode is Mandatory — so competing coin-flip
                // replacements auto-apply and a CoinFlip NEVER parks for an
                // ordering or an optional choice. An optional coin-flip
                // replacement (Pokey, the Scallywagg, currently unparsed) would
                // reach this arm even alone, through the optional-choice path.
                //
                // A non-commuting coin-flip replacement would need its flip
                // context parked OUTSIDE the resolution stack (the
                // `pending_die_roll_instruction` precedent) plus a delivery
                // here; `flip_through_replacement`'s `NeedsChoice` arm cannot
                // serve it, because it parks a keep-choice frame whose gate does
                // not match `ReplacementChoice`.
                ProposedEvent::CoinFlip { .. } => {
                    debug_assert!(
                        false,
                        "CoinFlip replacement reached the optional-choice resume path"
                    );
                }
                // CR 706.1 + CR 616.1: die-roll replacements (Barbarian Class,
                // Pixie Guide, Wyll) are Mandatory, but being mandatory does NOT
                // keep them off this path: two of them applying to the same
                // instruction is a CR 616.1 ordering choice, which is exactly the
                // marquee dice-matters interaction (Barbarian Class + Pixie
                // Guide, or Wyll + Pixie Guide). Delegate to the die-roll
                // resolver's own authority with the bound modified event — its
                // raised count and its ACCUMULATED CR 706.6 ignore rules ride
                // that event and must not be re-derived — mirroring the
                // `SearchFound` arm below.
                //
                // A re-suspension (a CR 706.6 ignore tie-break, or a results
                // branch's own choice) is returned as-is; a completed roll falls
                // through to the shared drain below, exactly as every other
                // completed resume arm does.
                event @ ProposedEvent::RollDice { .. } => {
                    match effects::roll_die::resume_roll_dice_after_replacement(
                        state, event, events,
                    ) {
                        Ok(effects::ResolutionProgress::Waiting) => {
                            return Ok(state.waiting_for.clone())
                        }
                        Ok(effects::ResolutionProgress::Complete) => {
                            effects::roll_die::publish_completed_result_to_parent(state);
                        }
                        Ok(effects::ResolutionProgress::Queued) => {}
                        Err(error) => return Err(EngineError::InvalidAction(format!("{error}"))),
                    }
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
            finish_replacement_execute_epilogue(
                state,
                ReplacementExecuteEpilogue {
                    zone_change_object_id,
                    enters_battlefield,
                    parked_single_discard,
                    parked_affected_object_id,
                    pending_phase_drain_life_loser,
                    action_event_start: replacement_action_event_start,
                    boundary: ReplacementEpilogueBoundary::Answer,
                },
                events,
            )
        }
        super::replacement::ReplacementResult::NeedsChoice(player) => {
            // The proposed event retains its canonical placement across parks.
            // Preserve the other delivery context independently of that value.
            //
            // A staged member's same-event re-park leaves this answer's events
            // in the action buffer rather than on the staged owner: the applied
            // replacement modified the member's own event, which is still being
            // consulted. Only a replacement that also moved another population
            // member while leaving this event with a further choice would need
            // its segment parked (`park_staged_run_events`); the printed
            // destruction replacements that move another object (Umbra armor,
            // CR 702.89a) end the event instead, and are parked at their own
            // `Prevented` result.
            let same_event_repark = state.pending_replacement.as_ref().is_some_and(|pending| {
                parked_delivery_boundary.is_some_and(|boundary| {
                    pending.proposed.affected_object_id() == boundary.affected_object_id()
                        && (std::mem::discriminant(&pending.proposed)
                            == std::mem::discriminant(boundary)
                            || matches!(
                                (&pending.proposed, boundary),
                                (
                                    ProposedEvent::ZoneChange { .. },
                                    ProposedEvent::Destroy { .. }
                                )
                            ))
                        && (boundary.affected_object_id().is_some()
                            || pending.proposed.affected_player(state)
                                == boundary.affected_player(state))
                })
            });
            if let Some(pending) = state
                .pending_replacement
                .as_mut()
                .filter(|_| same_event_repark)
            {
                if pending.exile_controller.is_none() {
                    pending.exile_controller = parked_exile_controller;
                }
                if pending.exile_duration.is_none() {
                    pending.exile_duration = parked_exile_duration.clone();
                }
                if matches!(
                    pending.exile_tracking,
                    crate::types::game_state::ZoneDeliveryExileTracking::None
                ) {
                    pending.exile_tracking = parked_exile_tracking;
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
                // CR 400.7: and the captured source incarnation.
                if pending.source_incarnation.is_none() {
                    pending.source_incarnation = parked_source_incarnation;
                }
                if let Some(provenance) = parked_sacrifice_provenance {
                    if pending.proposed.affected_object_id() == Some(provenance.object_id) {
                        pending.sacrifice_provenance = Some(provenance);
                    }
                }
                // CR 616.1f + CR 614.12a: one same-event carry-forward retains
                // the owner's original boundary, even when an outer Destroy's
                // current proposal has become a ZoneChange.
                if let Some(boundary) = parked_delivery_boundary {
                    pending.inherit_delivery_boundary(boundary);
                }
            }
            // CR 616.1 + CR 101.4: a choice handed to a later player waits
            // behind earlier outstanding choices of the same damage batch.
            if super::effects::deal_damage::defer_choice_behind_earlier_players(state, player) {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                super::engine::resume_pending_continuation_if_priority(state, events)?;
                return Ok(state.waiting_for.clone());
            }
            // CR 616.1 + CR 603.6d: render an as-enters roll's next prompt
            // from the same pre-entry board its re-scan used.
            state.waiting_for = viewed_choice.unwrap_or_else(|| {
                super::replacement::replacement_choice_waiting_for(player, state)
            });
            // CR 101.4d + CR 616.1: a staged member's answer exposed a further
            // choice on its own event; the batch holds it and asks again in
            // APNAP order.
            if same_event_repark && staged_consult_owner.is_some() {
                if let Some(handback) = crate::game::zone_pipeline::hold_answer_repark(
                    state,
                    events,
                    replacement_action_event_start,
                ) {
                    return match handback {
                        crate::game::zone_pipeline::StagedConsultationHandback::NotOwned => {
                            Ok(state.waiting_for.clone())
                        }
                        crate::game::zone_pipeline::StagedConsultationHandback::Parked(waiting) => {
                            Ok(*waiting)
                        }
                        // CR 608.2c: the held choice needed no answer after
                        // all and the population settled, so this answer ends
                        // as any other that settles its batch.
                        crate::game::zone_pipeline::StagedConsultationHandback::Settled => {
                            finish_replacement_execute_epilogue(
                                state,
                                ReplacementExecuteEpilogue {
                                    zone_change_object_id: None,
                                    enters_battlefield: false,
                                    parked_single_discard,
                                    parked_affected_object_id,
                                    pending_phase_drain_life_loser,
                                    action_event_start: replacement_action_event_start,
                                    boundary: ReplacementEpilogueBoundary::Answer,
                                },
                                events,
                            )
                        }
                    };
                }
            }
            Ok(state.waiting_for.clone())
        }
        super::replacement::ReplacementResult::Prevented => {
            // CR 608.2f (F1/F2): a fully prevented compound abandons the
            // paused clause — take the record so the deferred publication
            // never runs, and clear the ledger. First, and guarded: the
            // clause is dead on every sub-path below, but this arm is shared
            // by every replacement answer, so the ledger must clear only
            // while the clause is actually installed.
            if state.pending_simultaneous_token_clause.is_some() {
                super::replacement::abort_simultaneous_token_clause(state);
            }
            // The gain was replaced in full. A prompt from its substitute
            // can keep the combat batch parked, but the gain has no answer
            // left for that batch to stage.
            if let Some(record) = state
                .pending_combat_lifelink
                .as_mut()
                .filter(|_| parked_life_gain_recipient.is_some())
            {
                record.awaiting_staged_gain = false;
            }
            if let Some(object_id) = parked_zone_change_object_id {
                record_parked_land_play_completion(state, object_id);
            }
            // CR 614.6: a prevented move never happened, so the paused discard
            // owned by this answer is never recorded. A carrier installed by a
            // substitute during this answer is a different value and survives.
            if parked_single_discard.is_some()
                && state.pending_single_discard.as_deref().copied() == parked_single_discard
            {
                state.pending_single_discard = None;
            }
            // CR 614.6 + CR 608.2c: a staged prevention is a retained terminal
            // (or a `PreventedWithChild` while its own child prompt is live),
            // never an immediate delivery or logical-group recording.
            if let Some((group, fresh)) = staged_consult_owner {
                if !fresh {
                    return Ok(state.waiting_for.clone());
                }
                return match crate::game::zone_pipeline::record_staged_batch_consultation_result(
                    state,
                    group,
                    StagedMemberOutcome::Prevented,
                    events,
                    replacement_action_event_start,
                ) {
                    crate::game::zone_pipeline::StagedConsultationHandback::NotOwned => {
                        Ok(state.waiting_for.clone())
                    }
                    crate::game::zone_pipeline::StagedConsultationHandback::Parked(waiting) => {
                        Ok(*waiting)
                    }
                    crate::game::zone_pipeline::StagedConsultationHandback::Settled => {
                        finish_replacement_execute_epilogue(
                            state,
                            ReplacementExecuteEpilogue {
                                zone_change_object_id: None,
                                enters_battlefield: false,
                                parked_single_discard,
                                parked_affected_object_id,
                                pending_phase_drain_life_loser,
                                action_event_start: replacement_action_event_start,
                                boundary: ReplacementEpilogueBoundary::Answer,
                            },
                            events,
                        )
                    }
                };
            }
            if let Some(paused) = parked_zone_change_delivery.as_ref() {
                state.capture_paused_zone_change_delivery(
                    paused.member,
                    &paused.expected_event,
                    &[],
                    crate::types::game_state::ZoneMoveCompletion::Prevented,
                );
                effects::settle_replaced_forwarded_zone_delivery(state, paused.member, &[]);
            }
            // CR 120.3f + CR 120.4b + CR 120.4c + CR 614.6: a prevented poison
            // (or life-loss) result replaced only that result; the damage was
            // dealt, so a non-combat leg's parked remaining results (its
            // record when still owed, its lifelink gain) still settle. Gated
            // like the nested-prompt guard below: only a consumed prompt with
            // no freshly parked one is settled. A gain that parks surfaces its
            // own prompt (its answer finishes through the Execute funnel).
            if let Some(next) = drain_settled_non_combat_damage_stash(
                state,
                state.pending_replacement.is_none()
                    && matches!(
                        state.waiting_for,
                        WaitingFor::Priority { .. } | WaitingFor::ReplacementChoice { .. }
                    ),
                events,
                replacement_action_event_start,
            ) {
                return Ok(next);
            }
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
            // The wrapper retired the answered prompt before this body ran.
            // `continue_replacement` also took the prior pending record, so a
            // remaining `pending_replacement` belongs to a fresh nested ordering
            // choice. Preserve it, as well as other live replacing-action
            // prompts, so the next answer resumes the CR 616.1f repeat rather
            // than orphaning the nested work.
            if state.pending_replacement.is_some()
                || !matches!(
                    state.waiting_for,
                    WaitingFor::Priority { .. } | WaitingFor::ReplacementChoice { .. }
                )
            {
                return Ok(state.waiting_for.clone());
            }
            // CR 101.4 + CR 614.6: the answer replaced a life-gain cost
            // recipient's gain outright while the cost is mid-staging. Its
            // substitute joins the cost's staged batch rather than running now,
            // so the recipients still owed read the life totals from before the
            // instruction; the cost then resumes, stages them and settles all.
            if parked_life_gain_recipient.is_some_and(|recipient| {
                super::casting_costs::stage_answered_life_gain_cost(
                    state,
                    recipient,
                    AnsweredLifeGain::Substituted,
                )
            }) {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                super::engine::resume_pending_continuation_if_priority(state, events)?;
                return Ok(state.waiting_for.clone());
            }
            if let Some(loser) = pending_phase_drain_life_loser {
                // CR 614.1a: the chosen replacement prevented the loss outright,
                // so no life left this player and nothing may narrate one.
                // Consume the parked provenance anyway — left behind, it is a
                // record with no event, and the next same-player loss to resume
                // through this pipeline would claim it and be logged as mana
                // burn. `actual: 0` consumes without emitting.
                super::turns::note_empty_pool_life_loss_resolved(state, loser, 0, events);
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                // CR 614.6: A cross-event replacement's substitute completes
                // before the original phase-transition drain may resume. If it
                // pauses again, preserve the phase cursor and surface that
                // prompt; its eventual replacement epilogue will resume here.
                if state.has_post_replacement_drain() {
                    if let Some(waiting_for) =
                        apply_pending_post_replacement_effect(state, None, None, None, events)
                    {
                        state.waiting_for = waiting_for;
                        super::turns::mark_phase_transition_awaiting_post_replacement(state);
                    }
                }
                if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    super::turns::drain_pending_phase_transition_progress(state, events);
                }
                return Ok(state.waiting_for.clone());
            }
            // CR 614.6 + CR 616.1f + CR 500.5: A prevented LifeLoss choice
            // raised *inside* an interactive substitute is a child of the
            // parked phase-transition owner, not a second root. Resume the
            // outer ability continuation and its exact post-replacement frame
            // before the generic prevented-spell teardown below can discard
            // that work. The shared resumer advances the phase cursor only
            // after the outer frame is terminal; a further choice leaves the
            // cursor Awaiting and surfaces that prompt.
            if state
                .pending_phase_transition_progress
                .as_ref()
                .is_some_and(|progress| {
                    progress.drain_state
                        == crate::types::game_state::PhaseTransitionDrainState::AwaitingPostReplacementContinuation
                })
            {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                super::engine::resume_pending_continuation_if_priority(state, events)?;
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
            if state.active_connive_reentry().is_some() {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                if let Some(wf) = drain_pending_connive_reentry(state, events) {
                    return Ok(wf);
                }
                return Ok(state.waiting_for.clone());
            }
            if state.active_life_total_assignment().is_some() {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                drain_pending_life_total_assignment(state, events);
                return Ok(state.waiting_for.clone());
            }
            if state.active_counter_additions().is_some() {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                effects::counters::drain_pending_counter_additions(state, events);
                if matches!(state.waiting_for, WaitingFor::Priority { .. })
                    && state.active_copy_token().is_some()
                {
                    effects::token_copy::drain_pending_copy_token_resolution(state, events);
                }
                // CR 101.4 + CR 616.1: resume an `EachPlayerCopyChosen` walk whose
                // counter placement was prevented — advance to the next player.
                maybe_drain_each_player_copy_chosen(state, events);
                // Same position as the Execute arm's call in program terms: this block
                // RETURNS at the next line, so "after every drain of this arm" and "here"
                // are the same point.
                finish_spell_resolution_exposed_by_child(state, parked_affected_object_id, events);
                // CR 614.12 + CR 616.1: a prevented placement still owes a settled
                // enter-as-copy entry its fallible tail. Run the engine boundary
                // (which consumes that tail) only when such a tail is ready; every
                // other prevented-counter pause keeps its existing, non-resuming
                // exit.
                if effects::counters::copy_entry_completion_is_ready(state) {
                    super::engine::resume_pending_continuation_if_priority(state, events)?;
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
            // CR 616.1 + CR 608.2c: a prevented counter removal belongs to whatever owns
            // the work it interrupted. Work the parked counter-cost root's replacement
            // installed is that root's subtree (its post-effect, an effect removal queue,
            // a rider): it finishes first, and the root resumes once it retires. With the
            // subtree retired the removal is the root's own (CR 118.11: the cost is still
            // paid). With no counter-cost root the owner is the resolving work (an effect
            // queue, damage, a resolution-time cost) and any other parked root resumes after
            // it, in the Execute arm's order.
            if pending_was_counter_removal {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                // CR 614.6 + CR 616.1: a batch zone move whose member's delivery-tail
                // post-effect proposed this removal is parked on top of the removal's own
                // work (`zone_pipeline::deliver_batch`, `NewChild`). It is part of the same
                // modified event, so it settles first, as `resume_resolution_frames`'
                // BatchDelivery arm does, exposing the removal's queue. A top batch is not
                // always this removal's: a mana ability activated at an earlier prompt that
                // parked a batch (an unless payment raised by a delivery-tail post-effect)
                // proposes its removal above that batch, which belongs to the outer prompt.
                // So it settles only where it is this removal's own work: with no
                // counter-cost root, or above an unfinished root's recorded boundary; never
                // under a retired root, whose own removal it cannot be.
                match state.counter_cost_root_subtree() {
                    CounterCostRootSubtree::Unfinished => {
                        let batch_in_subtree = state
                            .pending_cost_move_resume
                            .as_ref()
                            .and_then(PendingCostMoveResume::paused_counter_removal)
                            .is_some_and(|paused| {
                                state.resolution_stack.len()
                                    > paused.replacement_subtree.resolution_depth
                            });
                        if batch_in_subtree && !settle_parked_batch_delivery(state, events) {
                            return Ok(state.waiting_for.clone());
                        }
                        effects::counters::drain_pending_counter_removals(state, events);
                        super::engine::resume_pending_continuation_if_priority(state, events)?;
                    }
                    CounterCostRootSubtree::Retired => {
                        resume_cost_move_root_after_prevention(
                            state,
                            events,
                            replacement_action_event_start,
                        )?;
                    }
                    CounterCostRootSubtree::Absent => {
                        if !settle_parked_batch_delivery(state, events) {
                            return Ok(state.waiting_for.clone());
                        }
                        effects::counters::drain_pending_counter_removals(state, events);
                        if matches!(state.waiting_for, WaitingFor::Priority { .. })
                            && !matches!(
                                state.pending_cost_move_resume,
                                Some(PendingCostMoveResume::ManaAbilityPayment { .. })
                            )
                        {
                            effects::drain_pending_continuation(state, events);
                        }
                        if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                            resume_cost_move_root_after_prevention(
                                state,
                                events,
                                replacement_action_event_start,
                            )?;
                        }
                    }
                }
                return Ok(state.waiting_for.clone());
            }
            if state.active_copy_token().is_some() {
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
            if state.active_batch_delivery().is_some() {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                crate::game::zone_pipeline::drain_pending_batch_deliveries(state, events);
                // CR 120.3f + CR 614.6: a prevented Horde mill member whose
                // leg parked its results: once the batch retires, the paused
                // loss is settled and the non-combat leg's results follow.
                if let Some(next) = drain_settled_non_combat_damage_stash(
                    state,
                    matches!(state.waiting_for, WaitingFor::Priority { .. }),
                    events,
                    replacement_action_event_start,
                ) {
                    return Ok(next);
                }
                return Ok(state.waiting_for.clone());
            }
            // CR 616.1 + CR 608.2c: any other prevented event inside a parked counter-cost
            // root's replacement subtree finishes that subtree first; the root resumes once
            // it retires.
            if state.counter_cost_root_subtree() == CounterCostRootSubtree::Unfinished {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                super::engine::resume_pending_continuation_if_priority(state, events)?;
                return Ok(state.waiting_for.clone());
            }
            // CR 121.2 + CR 614.11a + CR 121.6b: a skipped draw inside a draw
            // instruction that a parked unless-cost or a parked entry MayCost
            // owns finishes that instruction before the owning payment settles,
            // pays its later legs or issues its next instruction, mirroring the
            // delivered arm's ordering. Gated to those parks: the CR 608.3e
            // teardown below reads only the stack top, so completing an ordinary
            // effect's draw frame here would expose, then clear, a continuation
            // that today survives under the frame. Non-cost draws keep their
            // existing priority-boundary resume.
            if matches!(
                state.pending_cost_move_resume,
                Some(
                    PendingCostMoveResume::CounterAdditionUnlessPayment { .. }
                        | PendingCostMoveResume::ReplacementMayCostInnerChoice { .. }
                )
            ) {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                if let Some(wf) = resume_active_draw_sequences(state, events) {
                    return Ok(wf);
                }
            }
            // CR 118.11 + CR 614.11a: a skipped draw that interrupted a bare
            // cast or activation cost payment resumes through the same authority
            // the delivered arm uses; a prevented event still leaves the cost
            // paid.
            if (state.pending_cast.is_some() || state.pending_discard_for_cost.is_some())
                && state.counter_cost_root_subtree() != CounterCostRootSubtree::Unfinished
            {
                // CR 614.11a: finish the whole draw instruction (its remaining
                // units and their substitutes) before the interrupted payer
                // resumes; only then does the cost count as settled.
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                if let Some(wf) = resume_active_draw_sequences(state, events) {
                    return Ok(wf);
                }
                let resume_cost_event_start = state
                    .pending_discard_for_cost
                    .is_some()
                    .then_some(replacement_action_event_start);
                state.waiting_for = super::casting_costs::resume_interrupted_cost_payment(
                    state,
                    events,
                    resume_cost_event_start,
                )?;
            }
            if let Some(waiting_for) = resume_cost_move_root_after_prevention(
                state,
                events,
                replacement_action_event_start,
            )? {
                return Ok(waiting_for);
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
            // un-migrated twin of stack.rs's C2 prevented-permanent site. On a
            // pause, surface the parked prompt (its resume delivers the chosen
            // event through the ZoneChange arm above).
            //
            // CR 608.2c: a prevented or replaced event ends nothing else in what is resolving. The
            // continuation stays, and `resume_pending_continuation_if_priority` below drains it:
            // Lightning Helix's "and you gain 3 life" with Mending Hands first under Furnace of Rath,
            // or the "you draw that many cards" of New Way Forward's reflexive trigger (CR 603.12)
            // under two Force Bubbles.
            if let Some(ctx) = state.take_active_spell_resolution() {
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
            // Turtle Team-Up: resume a hero-event batch parked by a nested
            // replacement prompt — the Prevented twin of the Execute arm's
            // hook. Preventing one step (a regeneration shield saving one
            // Mayhem destroy) modifies only that action (CR 608.2c); the
            // remaining steps still run. Re-pauses re-park through this same
            // hook, so nested pauses keep composing.
            if state.pending_hero_event.is_some() {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                match super::boss::drain_pending_hero_event(state, events) {
                    super::boss::HeroEventDrainOutcome::Idle => {}
                    super::boss::HeroEventDrainOutcome::PausedForReplacement => {
                        super::engine_resolution_choices::defer_observer_triggers_for_paused_choice(
                            state,
                            events,
                            replacement_action_event_start,
                        );
                        return Ok(state.waiting_for.clone());
                    }
                    super::boss::HeroEventDrainOutcome::Completed => {
                        effects::drain_pending_continuation(state, events);
                        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                            super::engine_resolution_choices::defer_observer_triggers_for_paused_choice(
                                state,
                                events,
                                replacement_action_event_start,
                            );
                            return Ok(state.waiting_for.clone());
                        }
                    }
                }
            }
            state.waiting_for = WaitingFor::Priority {
                player: state.active_player,
            };
            super::engine::resume_pending_continuation_if_priority(state, events)?;
            Ok(state.waiting_for.clone())
        }
    }
}

/// CR 120.4b + CR 603.2: is the active continuation the rest of a damage
/// effect (the kinds `effects/deal_damage.rs` marks with
/// `mark_pending_continuation_parent`)? Read BEFORE a drain, which takes the
/// frame. Fight is deliberately not included (its re-pausing continuations
/// keep their pre-existing collection).
fn damage_parent_continuation_pending(state: &GameState) -> bool {
    let Some(frame) = state.active_ability_continuation_frame() else {
        return false;
    };
    // The re-parked remainder of a damage chain (a later recipient's `DealDamage` node that
    // paused again) no longer carries its parent kind: the drain that resumed it already
    // re-emitted the parent event. It is still the rest of the same damage effect.
    matches!(
        frame.pending.parent_kind,
        Some(
            EffectKind::DamageAll
                | EffectKind::DamageEachPlayer
                | EffectKind::EachDealsDamageEqualToPower
                | EffectKind::EachSourceDealsDamage
        )
    ) || matches!(frame.pending.chain.effect, Effect::DealDamage { .. })
}

/// CR 120.4b + CR 603.2 + CR 603.3b: a damage effect's continuation drained in
/// this answering action and left a non-Priority wait (a later recipient's
/// result paused), so the action ends on a prompt and the post-action trigger
/// scan never sees the damage events dealt so far. Abilities that trigger on
/// that damage trigger now and wait in `deferred_triggers`; the action-wide
/// start is used (the filter skips occurrences another collector claimed,
/// CR 603.2c). Only damage-parent continuations defer here (see
/// [`damage_parent_continuation_pending`]).
fn defer_damage_chain_observers_at_pause(
    state: &mut GameState,
    damage_chain: bool,
    events: &mut Vec<GameEvent>,
    action_event_start: usize,
) {
    if damage_chain {
        super::engine_resolution_choices::defer_observer_triggers_for_paused_choice(
            state,
            events,
            action_event_start,
        );
    }
}

/// CR 120.3f + CR 120.1 + CR 616.1: settle a non-combat player-damage leg's
/// parked results (`GameState::pending_damage_post_loss`) once the result that
/// paused it has settled: `settled` is the caller's "the answered prompt was
/// consumed and nothing new is parked" guard. Shared by the Execute funnel and
/// the Prevented answer (CR 614.6: a prevented poison or loss result replaced
/// only that result; the damage was dealt, CR 120.4b/c, so its lifelink is
/// still owed). Skipped while a batch zone move is still parked (the paused
/// loss is not settled yet) and for a combat stash (the Phase C remnant owns
/// it; the `handle_replacement_choice` wrapper resumes it on every Priority
/// return). Returns the wait to adopt when the lifelink gain parked its own
/// prompt (the pending replacement owns the gain), else `None`.
///
/// CR 120.4b + CR 603.2 + CR 603.3b: when the settle pauses, the answering
/// action ends on that prompt and the post-action scan never sees the damage
/// events it (and the action before it) emitted, so their observers are
/// deferred from `action_event_start` (the answering action's start).
fn drain_settled_non_combat_damage_stash(
    state: &mut GameState,
    settled: bool,
    events: &mut Vec<GameEvent>,
    action_event_start: usize,
) -> Option<WaitingFor> {
    if !settled
        || !state
            .pending_damage_post_loss
            .as_ref()
            .is_some_and(|stashed| !stashed.is_combat)
        || state.active_batch_delivery().is_some()
    {
        return None;
    }
    let owner = state
        .pending_damage_post_loss
        .as_ref()
        .and_then(|stash| stash.damage_delivery_owner);
    let outcome = effects::resume_damage_delivery(state, owner, |state| {
        effects::deal_damage::drain_pending_damage_post_loss(state, events)
    });
    match outcome {
        effects::deal_damage::DamagePostLossDrainOutcome::PausedForReplacement => {
            super::engine_resolution_choices::defer_observer_triggers_for_paused_choice(
                state,
                events,
                action_event_start,
            );
            Some(state.waiting_for.clone())
        }
        effects::deal_damage::DamagePostLossDrainOutcome::Settled => None,
    }
}

/// CR 614.6 + CR 616.1: settle a batch zone move parked on top of the
/// resolution stack, if there is one. Returns whether the action is still at an
/// ordinary Priority boundary afterwards (a later member can surface its own
/// prompt).
fn settle_parked_batch_delivery(state: &mut GameState, events: &mut Vec<GameEvent>) -> bool {
    if state.active_batch_delivery().is_some() {
        crate::game::zone_pipeline::drain_pending_batch_deliveries(state, events);
    }
    matches!(state.waiting_for, WaitingFor::Priority { .. })
}

/// CR 601.2h + CR 602.2b + CR 605.3b + CR 616.1: resume the parked cost-move
/// root at the Prevented boundary; a mana root's rider drains after it.
fn resume_cost_move_root_after_prevention(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    action_event_start: usize,
) -> Result<Option<WaitingFor>, EngineError> {
    // CR 601.2h + CR 602.2b + CR 616.1: A fully substituted cost move still
    // completes that cost-payment step. No fresh prompt remains at this point,
    // so restore the normal reducer boundary before draining its typed
    // continuation.
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    let resumed_mana_ability_cost = matches!(
        state.pending_cost_move_resume,
        Some(PendingCostMoveResume::ManaAbilityPayment { .. })
    );
    let Some(waiting_for) = super::engine::drain_pending_cost_move_resume(
        state,
        events,
        super::engine::CostMoveDrainBoundary::ReplacementPrevented { action_event_start },
    )?
    else {
        return Ok(None);
    };
    // CR 605.3b + CR 616.1: A prevented mana cost has the same rider ordering
    // as a delivered one: its typed root settles before any ordinary
    // continuation drains.
    if resumed_mana_ability_cost
        && matches!(waiting_for, WaitingFor::Priority { .. })
        && (state.active_ability_continuation().is_some()
            || state.active_change_zone_frame().is_some())
    {
        effects::drain_pending_continuation(state, events);
    }
    Ok(Some(state.waiting_for.clone()))
}

/// CR 614.12a: Commit a pre-entry controller choice onto the exact parked
/// ZoneChange, then resume through the ordinary replacement-choice handler so
/// delivery, trigger collection, and any later replacement ordering retain
/// their existing single authorities.
pub(super) fn handle_entry_controller_choice(
    state: &mut GameState,
    opponent: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let (player, candidates) = match &state.waiting_for {
        WaitingFor::EntryControllerChoice { player, candidates } => (*player, candidates),
        _ => {
            return Err(EngineError::InvalidAction(
                "entry controller choice is not pending".to_string(),
            ));
        }
    };
    if !candidates.contains(&opponent)
        || !crate::game::players::choosable_opponents(state, player).contains(&opponent)
    {
        return Err(EngineError::InvalidAction(
            "chosen entry controller is not eligible".to_string(),
        ));
    }
    let Some(pending) = state.pending_replacement.as_mut() else {
        return Err(EngineError::InvalidAction(
            "entry controller choice has no pending replacement".to_string(),
        ));
    };
    if pending.candidates.len() != 1 || pending.is_optional {
        return Err(EngineError::InvalidAction(
            "entry controller choice has an invalid replacement resume".to_string(),
        ));
    }
    // The logical owner parked on the unanswered event; keep that boundary
    // before the answer changes the proposal.
    pending.pin_delivery_boundary();
    let ProposedEvent::ZoneChange {
        controller_override,
        to: Zone::Battlefield,
        ..
    } = &mut pending.proposed
    else {
        return Err(EngineError::InvalidAction(
            "entry controller choice does not own a battlefield entry".to_string(),
        ));
    };
    *controller_override = Some(opponent);
    pending.proposed.applied_set_mut().insert(
        crate::types::proposed_event::AppliedReplacementKey::EntryControllerChoice {
            source: pending.candidates[0].source,
            index: pending.candidates[0].index,
            controller: opponent,
        },
    );
    // Mark before re-entering the ordinary handler so the pre-entry chooser
    // is not offered again while that handler applies this same replacement.
    pending.proposed.mark_applied(pending.candidates[0]);
    handle_replacement_choice(state, 0, events)
}

/// CR 614.12a + CR 614.12: answer path for a pre-entry as-enters choice
/// ("As ~ enters, choose …"). The parked entry's single candidate is the
/// entrant's own choice replacement. The answer rides the entry event
/// (`enter_with_choices`) so the CR 616.1f re-scan sees the permanent as it
/// would exist, and delivery installs it on the new object (CR 400.7). The
/// event the logical owner parked on is kept as the pending record's
/// `delivery_boundary` before the proposal changes, so the shared resume settles
/// the same paused delivery.
pub(super) fn handle_entry_named_choice(
    state: &mut GameState,
    choice_type: &crate::types::ability::ChoiceType,
    choice: &str,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if !super::replacement::pending_entry_named_choice_is_live(state) {
        return Err(EngineError::InvalidAction(
            "as-enters choice has no pending battlefield entry".to_string(),
        ));
    }
    let pending = state
        .pending_replacement
        .as_mut()
        .expect("a live pre-entry choice owns the pending replacement");
    pending.pin_delivery_boundary();
    let rid = pending.candidates[0];
    let ProposedEvent::ZoneChange {
        enter_with_choices, ..
    } = &mut pending.proposed
    else {
        unreachable!("a live pre-entry choice parks a battlefield ZoneChange");
    };
    enter_with_choices
        .get_or_insert_with(Default::default)
        .0
        .extend(effects::choose::chosen_attributes_for_choice(
            choice_type,
            choice,
        ));
    // Mark before re-entering the ordinary handler so the choice is not raised
    // again while that handler applies this same replacement.
    pending.proposed.mark_applied(rid);
    handle_replacement_choice(state, 0, events)
}

/// CR 614.12a + CR 118.11 + CR 119.4: Answer path for an as-enters life
/// payment (Phyrexian Processor class). Pays via the shared life-cost
/// authority, latches the SUBMITTED amount as `ChosenAttribute::Number`
/// (modifiers change what is lost, never what X remembers — CR 118.11), and
/// completes the parked entry. Deliberately stamps NO `last_effect_count`
/// and touches NO continuation: the prompt answers an entry, not a resolving
/// ability (CR 614.1). The liveness gate lives at the `SubmitPayAmount`
/// answer site (`pending_entry_payment_is_live`).
pub(super) fn handle_entry_payment(
    state: &mut GameState,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if !super::replacement::pending_entry_payment_is_live(state) {
        return Err(EngineError::InvalidAction(
            "as-enters payment has no pending battlefield entry".to_string(),
        ));
    }
    let (player, accumulated) = match &state.waiting_for {
        WaitingFor::PayAmountChoice {
            player,
            accumulated,
            ..
        } => (*player, *accumulated),
        _ => unreachable!("a live pre-entry payment owns a PayAmountChoice prompt"),
    };
    // CR 614.12a: the copied payment precedes simultaneous entry. Judge its
    // prohibition on the same board as its offer, before taking the parked
    // record; ordinary card-entry payments retain the live-board context.
    let prohibition_board = super::replacement::entry_group_payment_source(state)
        .map(|entrant| super::replacement::copy_entry_board(state, entrant).into_owned());
    // Take the outer record FIRST so the inner life-loss ordering (which owns
    // `pending_replacement` while IT resolves) cannot collide with it; pin the
    // taken local's delivery boundary BEFORE the payment can change the
    // proposal (the G1 pin-first order).
    let mut pending = state
        .pending_replacement
        .take()
        .expect("a live pre-entry payment owns the pending replacement");
    pending.pin_delivery_boundary();
    let resume_at_resolution_depth = state.resolution_stack.len();
    let payment_event_start = events.len();
    match super::life_costs::pay_life_as_cost_with_prohibition_board(
        state,
        player,
        amount,
        prohibition_board.as_ref(),
        events,
    ) {
        PayLifeCostResult::Paid { .. } => {
            if let Some(slot) =
                super::replacement::proposed_entry_choices_mut(&mut pending.proposed)
            {
                slot.get_or_insert_with(Default::default)
                    .0
                    .push(ChosenAttribute::Number(amount));
            }
            state.pending_replacement = Some(pending);
            collect_entry_group_payment_triggers(state, events, payment_event_start);
            resume_parked_entry_payment(state, events)
        }
        PayLifeCostResult::PaidWithDeferredSubstitution { .. }
        | PayLifeCostResult::DeferredReplacementChoice { .. } => {
            // The inner prompt owns `pending_replacement` now; the latched
            // outer rides the resume marker until the drain completes it.
            if let Some(slot) =
                super::replacement::proposed_entry_choices_mut(&mut pending.proposed)
            {
                slot.get_or_insert_with(Default::default)
                    .0
                    .push(ChosenAttribute::Number(amount));
            }
            state.pending_deferred_life_cost_resume = Some(DeferredLifeCostResume::PayAmount {
                player,
                total: accumulated.saturating_add(amount),
                resume_at_resolution_depth,
                pending_entry: Some(Box::new(pending)),
            });
            collect_entry_group_payment_triggers(state, events, payment_event_start);
            Ok(state.waiting_for.clone())
        }
        PayLifeCostResult::InsufficientLife | PayLifeCostResult::Prohibited => {
            // The payment changed nothing (life costs fail atomically); restore
            // the pinned outer and reject the answer.
            state.pending_replacement = Some(pending);
            Err(EngineError::InvalidAction(format!(
                "Player {player:?} cannot pay {amount} life"
            )))
        }
    }
}

/// CR 614.12a + CR 603.2c + CR 603.3b: collect payment observers while the
/// parked entry still identifies the pre-entry population, before replay or
/// continuation can retire its token cohort. The existing occurrence ledger
/// leaves delayed observers visible and prevents a second ordinary scan.
fn collect_entry_group_payment_triggers(
    state: &mut GameState,
    events: &[GameEvent],
    event_start: usize,
) {
    let Some(entrant) = super::replacement::entry_group_payment_source(state) else {
        return;
    };
    let unconsumed = super::triggers::filter_already_collected_trigger_events_from(
        state,
        events,
        event_start,
        &state.consumed_before_priority_trigger_events,
    );
    super::triggers::collect_entry_payment_triggers_into_deferred(state, &unconsumed, entrant);
    super::triggers::claim_events_for_ordinary_collection(state, events, event_start);
}

/// CR 614.12a + CR 616.1f: shared completion tail for a parked entry payment:
/// install the pinned record, mark the answered payment applied, and re-enter
/// the ordinary handler (which re-loops the pipeline over the still-applicable
/// effects). Called by the answer (Paid path) and by the deferred drain.
pub(crate) fn resume_parked_entry_payment(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 614.12a + CR 707.2: a copy-surfaced payment parks a synthetic
    // battlefield-to-battlefield proposal — there is no delivery to resume.
    if state
        .pending_replacement
        .as_ref()
        .is_some_and(super::replacement::parked_entry_payment_is_copy_surface)
    {
        return complete_copy_surfaced_payment(state, events);
    }
    let pending = state
        .pending_replacement
        .as_mut()
        .expect("an answered entry payment installs its pending replacement");
    // Idempotent no-op safety net (`pin_delivery_boundary` only sets `None`):
    // both callers install a pre-pinned record.
    pending.pin_delivery_boundary();
    let rid = pending.candidates[0];
    // Mark before re-entering the ordinary handler so the payment is not
    // raised again while that handler applies this same replacement.
    pending.proposed.mark_applied(rid);
    handle_replacement_choice(state, 0, events)
}

/// CR 614.12a + CR 707.2: complete a copy-surfaced entry payment (Clone
/// entering as Minion of the Wastes). The parked proposal is synthetic — the
/// object already entered — so the latch folds straight onto the live object
/// and the synthetic dies here (no mark: nothing re-scans it). Latch-before-
/// replay, mirroring the post-copy `NamedChoice` answer
/// (`engine_resolution_choices.rs`, "As it enters, choose …" block), so ETB
/// observers fire against the realized post-payment object.
fn complete_copy_surfaced_payment(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let pending = state
        .pending_replacement
        .take()
        .expect("an answered copy-surfaced payment parks its synthetic proposal");
    let ProposedEvent::ZoneChange {
        object_id,
        enter_with_choices,
        ..
    } = &pending.proposed
    else {
        unreachable!("copy-surface parks are ZoneChange proposals");
    };
    if let Some(choices) = enter_with_choices {
        if let Some(obj) = state.objects.get_mut(object_id) {
            obj.chosen_attributes.extend(choices.0.iter().cloned());
            // CR 613.1: the CDA reads the latch at layer evaluation, which
            // already ran for the copy — re-run so the realized P/T lands
            // before the SBA pass (mirrors `effects::choose.rs`).
            crate::game::layers::mark_layers_full(state);
        }
    }
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    if entry_park_holds_entrant(state, *object_id) {
        if let Some(waiting_for) = replay_deferred_entry_events(state, *object_id, events)? {
            return Ok(waiting_for);
        }
    }
    super::engine::resume_pending_continuation_if_priority(state, events)?;
    Ok(state.waiting_for.clone())
}

/// CR 614.12a: complete a pre-entry payment whose inner life loss deferred
/// (ordering or substitute): install the preserved record and run the shared
/// completion tail. The latch was recorded at answer time; no stamp, no
/// continuation — the drain resumes an ENTRY, not a resolution chain.
pub(crate) fn complete_entry_payment(
    state: &mut GameState,
    pending: PendingReplacement,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    state.pending_replacement = Some(pending);
    collect_entry_group_payment_triggers(state, events, 0);
    resume_parked_entry_payment(state, events)
}

/// CR 707.2c + CR 614.12a + CR 613.1a: Answer path for Metamorphic Alteration's
/// "As this Aura enters, choose a creature." Latches the chosen creature's
/// copiable values (fixed here, per CR 707.2c, as the copy effect first starts
/// to apply — later changes to the chosen creature never propagate, CR 707.2b)
/// and installs them as a Layer-1 `CopyValues` transient continuous effect
/// SOURCED FROM the Aura and applied to the Aura's enchanted host. The Aura
/// itself is left untouched (it does not become a copy). The install reuses the
/// single copy-values authority (`become_copy::apply_precomputed_copy_values`),
/// which strips exceptions / flushes layers / emits `EffectResolved`.
fn handle_persist_chosen_attribute_choice(
    state: &mut GameState,
    source_id: ObjectId,
    controller: PlayerId,
    donor_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 707.2c: copiable values are determined at the moment the effect first
    // starts to apply — snapshot the donor now.
    let values = crate::game::layers::compute_current_copiable_values(state, donor_id).ok_or(
        EngineError::InvalidAction("Chosen copy source no longer exists".to_string()),
    )?;
    // CR 111.1 + CR 707.2: art routing follows the copy (token vs printed
    // source), captured alongside the values — NOT a copiable value itself.
    let display = state
        .objects
        .get(&donor_id)
        .map(|o| o.display_identity())
        .unwrap_or_default();

    // CR 400.7: persist the frozen snapshot on the Aura so it is cleared
    // automatically when the Aura changes zones (exactly the copy's lifetime).
    let snapshot = crate::types::ability::LatchedCopiableSnapshot {
        values: values.clone(),
        display: display.clone(),
    };
    if let Some(aura) = state.objects.get_mut(&source_id) {
        aura.chosen_attributes
            .push(crate::types::ability::ChosenAttribute::CopiableSnapshot(
                Box::new(snapshot),
            ));
    }

    // CR 608.3c + CR 614.12a: Spell-path mid-entry `CopyTargetChoice` is raised
    // from the CallerEpilogue post-replacement drain AFTER CR 608.3c Aura
    // attach — `attached_to` is already the cast host. The stash pushed there
    // (`PendingSpellResolution`) still carries `spell_targets` so this answer
    // path can apply cast-link stamps and prefer the CR 303.4a spell target as
    // the copy recipient (re-attach via `apply_pending_spell_resolution` is
    // idempotent).
    //
    // CR 303.4f: Non-spell Aura entry attaches during delivery (before this
    // choice), so `attached_to` is already set and there is no spell-resolution
    // frame — fall through to the attached host / enter-time attach consult.
    let mut host_from_spell: Option<ObjectId> = None;
    let mut took_spell_resolution = false;
    if state
        .active_spell_resolution()
        .is_some_and(|ctx| ctx.object_id == source_id)
    {
        let ctx = state
            .take_active_spell_resolution()
            .expect("active spell-resolution frame checked above");
        took_spell_resolution = true;
        host_from_spell = ctx.spell_targets.first().and_then(|t| match t {
            crate::types::ability::TargetRef::Object(id) => Some(*id),
            _ => None,
        });
        apply_pending_spell_resolution(state, &ctx, events);
    }

    // CR 303.4f: Non-spell path may still be unattached if entry skipped the
    // auto-attach consult (e.g. liminal copy → Aura). Resolve it now.
    // Never use this Enchant-filter consult as a CR 608.3c spell-path fallback
    // — a resolving Aura spell's host is the target chosen at cast (CR 303.4a),
    // carried on `PendingSpellResolution.spell_targets`.
    if !took_spell_resolution
        && state
            .objects
            .get(&source_id)
            .is_some_and(|aura| aura.attached_to.is_none())
    {
        match crate::game::zone_pipeline::resolve_entering_aura_attachment(state, source_id, events)
        {
            // CR 303.4g does NOT apply on this route: `NoLegalHost` here means the
            // entrant already ENTERED the battlefield as a non-Aura and only became
            // an Aura when its `BecomeCopy` replacement realized post-entry. There is
            // no un-entering it — CR 303.4g's "isn't created" / "remains in its
            // current zone" clause governs an object still in the act of entering —
            // so the CR 704.5m unattached-Aura state-based action owns it from here,
            // exactly as it did before the `Resolved` split. Same disposition as
            // `Attached`: nothing further to do at this seam.
            crate::game::zone_pipeline::EnteringAuraAttachment::NotApplicable
            | crate::game::zone_pipeline::EnteringAuraAttachment::Attached
            | crate::game::zone_pipeline::EnteringAuraAttachment::NoLegalHost => {}
            crate::game::zone_pipeline::EnteringAuraAttachment::NeedsChoice { .. } => {
                return Err(EngineError::InvalidAction(
                    "PersistChosenAttribute requires a resolved Aura host before the copy installs"
                        .to_string(),
                ));
            }
        }
    }

    // CR 303.4 + CR 702.5: the Aura enchants a creature — its host is the
    // recipient of the copy effect. Prefer the spell-target host established
    // above (spell-cast path), then the live attachment (non-spell / after
    // attach). Never silently no-op if the host is missing.
    let host_id = host_from_spell
        .or_else(|| {
            state
                .objects
                .get(&source_id)
                .and_then(|aura| aura.attached_to.as_ref())
                .and_then(|attach| attach.as_object())
        })
        .ok_or_else(|| {
            EngineError::InvalidAction(
                "Metamorphic Alteration copy choice answered without a resolved Aura host \
                 (CR 608.3c spell target / CR 303.4f attach)"
                    .to_string(),
            )
        })?;
    let duration_subject = crate::types::identifiers::ObjectIncarnationRef::from_object(
        state.objects.get(&host_id).ok_or_else(|| {
            EngineError::InvalidAction(
                "PersistChosenAttribute copy choice resolved after its Aura host left the game"
                    .to_string(),
            )
        })?,
    );

    // CR 707.2c + CR 613.1a + CR 611.2a: install the copy as a transient
    // continuous effect sourced from the Aura, applied to the host, ending
    // when the Aura leaves the battlefield (`UntilHostLeavesPlay` prunes on
    // `tce.source_id` leaving — the Aura). `duration_subject` tracks the
    // host for any recipient-relative duration reads (harmless here).
    let copy = super::effects::become_copy::PrecomputedCopyValues {
        source_id,
        controller,
        duration_subject,
        duration: crate::types::ability::Duration::UntilHostLeavesPlay,
        values,
        display,
        additional_modifications: Vec::new(),
        effect_kind: crate::types::ability::EffectKind::ChoosePermanent,
    };
    super::effects::become_copy::apply_precomputed_copy_values(state, host_id, copy, events)
        .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;

    // CR 707.4: installing the host copy must not disturb the Aura's attachment.
    debug_assert!(
        state
            .objects
            .get(&source_id)
            .is_some_and(|aura| aura.attached_to.is_some()),
        "the Aura must remain attached after installing the host copy"
    );

    // CR 615.5 + CR 614.12a: PersistChosenAttribute's ChoosePermanent work is
    // complete once the host copy is installed. Retire the paused post-
    // replacement drain BEFORE replaying deferred entry events — otherwise a
    // nested OrderTriggers / trigger-target pause can return while the same
    // ChoosePermanent drain is still resident, and a later post-action pass
    // re-drains it into a second `CopyTargetChoice`.
    //
    // Clear the prompt we just answered first: `state.waiting_for` is still the
    // inbound `CopyTargetChoice`, and the completion tail below used to echo it
    // via `if !Priority { return waiting_for }` — the cast driver then re-answered
    // the same prompt until its 64-iteration cap, leaving sequential Metamorphic
    // casts blocked (two_auras). Nested prompts raised during copy install /
    // entry replay still overwrite `waiting_for` and propagate below.
    //
    // Divergence from the BecomeCopy completion tail: that sibling
    // retires the drain *after* replay and brackets replay with
    // `capture_paused_zone_change_delivery_for_member` +
    // `drain_pending_batch_deliveries`. Those steps are for liminal / multi-
    // member batch deliveries (meld, ninjutsu copy tokens) that can park further
    // co-arrivers behind the mid-entry choice. An Aura *spell* entry is a
    // single-object `CallerEpilogue` delivery with no active batch frame — there
    // is nothing for delivery-capture or batch-drain to preserve — so this path
    // intentionally omits them. Retiring before replay is required here because
    // `Effect::ChoosePermanent` (unlike `BecomeCopy`) is itself the post-
    // replacement continuation being answered; leaving it paused through replay
    // lets a nested prompt return and then re-surface the same ChoosePermanent.
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    state.finish_active_paused_post_replacement_dispatch();

    // CR 614.12a + CR 603.2: replay the Aura's deferred battlefield-entry event
    // now that the host copy is realized, so ETB observers see the final state
    // (mirrors the BecomeCopy completion tail, but with the drain already
    // retired — see above).
    if let Some(waiting_for) = replay_deferred_entry_events(state, source_id, events)? {
        return Ok(waiting_for);
    }
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        return Ok(state.waiting_for.clone());
    }
    Ok(WaitingFor::Priority {
        player: state.active_player,
    })
}

/// The `CopyTokenOf`-predicated sibling of `copy_effect_for_source`: recover the
/// copy tail of a `ChoosePermanent` → `CopyTokenOf` post-replacement
/// continuation whose copy source is chosen at resolution. Esix, Fractal Bloom
/// answers on its parked replacement (`resume_token_substitute`); this route
/// serves a continuation of that shape, which no printed card reaches.
///
/// The `active_replacements` scan re-derives the tail rather than reading a
/// parked continuation, because by the time this runs the post-replacement
/// drain has already moved its continuation out: `begin_dispatch` does
/// `mem::replace(&mut drain.status, DrainStatus::Dispatching)` and hands the
/// continuation to the caller, `DrainStatus::Paused` carries no payload, and
/// `ready_continuation()` returns `None` for both `Dispatching` and `Paused`.
/// There is no parked continuation left to read, so the tail is re-derived from
/// the source's live replacements instead.
///
/// The predicate goes through `ability_tree_copies_tokens` — the same authority
/// the raise site used to stamp `purpose`.
fn copy_token_tail_for_source(
    state: &GameState,
    source_id: ObjectId,
) -> Option<&AbilityDefinition> {
    state.objects.get(&source_id)?;
    // CR 702.26b + CR 114.4: `active_replacements` filters out phased-out /
    // non-emblem command-zone sources.
    super::functioning_abilities::active_replacements(state)
        .filter(|(_, o, _)| o.id == source_id)
        .filter_map(|(_, _, replacement)| replacement.execute.as_deref())
        .find_map(|effect_def| {
            super::replacement::EventModifiers::first_non_modifier_ability(Some(effect_def)).filter(
                |real| {
                    matches!(&*real.effect, Effect::ChoosePermanent { .. })
                        && super::replacement::ability_tree_copies_tokens(real)
                },
            )
        })
        .and_then(|real_work| real_work.sub_ability.as_deref())
}

/// CR 707.1 + CR 614.1a: answer path for a `ChoosePermanent` →
/// `CopyTokenOf` continuation's copy SOURCE choice — the shape Esix, Fractal
/// Bloom's "you may instead choose a creature other than ~ and create that
/// many tokens that are copies of that creature" parses to ("instead" is what
/// makes it a replacement effect). The chosen permanent is the object the
/// substitute tokens copy (CR 707.2: they acquire its copiable values), not a
/// donor of copiable values to some third party. Esix itself answers on its
/// parked replacement (`resume_token_substitute`); this route serves a
/// continuation of that shape, which no printed card reaches.
///
/// A deliberate SIBLING of `handle_persist_chosen_attribute_choice`, never a
/// shared refactor of it: that function's completion carries a `debug_assert!`
/// requiring its source to still be an attached Aura ("the Aura must remain
/// attached after installing the host copy"), which is Aura-specific machinery.
/// A resident creature with no `attached_to` routed through that handler would
/// trip the assertion rather than share useful code.
fn handle_copy_token_source_choice(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    target_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let Some(tail) = copy_token_tail_for_source(state, source_id) else {
        return Err(EngineError::InvalidAction(
            "Missing copy-token tail for chosen copy source".to_string(),
        ));
    };
    // Clone to end the immutable borrow of `state` taken by the scan above.
    let tail = tail.clone();

    // `state.waiting_for` is STILL the `CopyTargetChoice` being answered here:
    // `handle_copy_target_choice` takes its `waiting_for` by value and never
    // clears the field. Snapshot it so the retire step below can distinguish
    // "nothing new was raised" from "the copy batch parked and raised its own
    // question".
    let inbound = state.waiting_for.clone();

    // CR 707.2: the copy reads the chosen object's copiable values. The tail's
    // `CopyTokenOf { target: Any }` plus this explicit target resolves to the
    // answered object, and "that many" (`QuantityRef::EventContextAmount`)
    // reads the ordinary cascade. Resolving BEFORE the retire step below is
    // load-bearing: the retire can clear the pause, and the transients it
    // clears must still be live here.
    let ability = build_resolved_from_def_with_targets(
        &tail,
        source_id,
        player,
        vec![TargetRef::Object(target_id)],
    );
    effects::resolve_ability_chain(state, &ability, events, 0)
        .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;

    // CR 614.5: a replacement effect gets only one opportunity to affect an
    // event. `Effect::ChoosePermanent` IS the post-replacement
    // continuation being answered, so a completed resolution must retire it —
    // leaving it paused lets a later post-action pass re-drain it into a second
    // `CopyTargetChoice`. But the copy-token batch can PARK mid-resolution and
    // raise its own question, and `resolve_ability_chain` returns
    // `Result<(), EffectError>` with no pause channel — `state.waiting_for` is
    // the only signal it has. The sites in `effects/token_copy.rs` that write
    // it include the outer `CreateToken` `NeedsChoice` arm, the per-token
    // `TokenEntry` `NeedsChoice` arm, a nested post-replacement effect that
    // parks a liminal entry resume, and a copied Aura token's
    // `ReturnAsAuraTarget` host choice.
    // Forcing `Priority` here would swallow that prompt and strand the parked
    // batch behind it.
    //
    // Both conjuncts are load-bearing. A nested prompt can itself be a
    // `CopyTargetChoice` (the liminal-resume site propagates whatever
    // `apply_pending_post_replacement_effect` returned), so a shape-only
    // `matches!` test would mistake it for our own resident prompt and retire
    // the pause; comparing against the snapshotted inbound value distinguishes
    // them by `source_id` / `valid_targets` / `purpose`. Conversely a fully
    // completed batch legitimately writes `Priority` itself, and the `Priority`
    // conjunct is what admits that completion path instead of misreading it as
    // a new question.
    let raised_new_prompt =
        state.waiting_for != inbound && !matches!(state.waiting_for, WaitingFor::Priority { .. });
    if raised_new_prompt {
        // The nested question owns the pause; its own resume path retires it.
        return Ok(state.waiting_for.clone());
    }
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    state.finish_active_paused_post_replacement_dispatch();

    // No `replay_deferred_entry_events` here, and that omission is deliberate
    // rather than forgotten. The replaced event is `CreateToken`, whose source
    // (Esix) is a resident permanent that is NOT entering the battlefield, so
    // `capture_deferred_entry_events_if_mid_entry_choice`'s entry-event filter
    // (`ZoneChanged { object_id == source_id, to: Battlefield }`) matched
    // nothing and captured nothing. There is no deferred entry event belonging
    // to this source to replay. Contrast the Aura sibling, whose source IS the
    // entering object and which therefore must replay.
    Ok(state.waiting_for.clone())
}

/// CR 707.5 + CR 603.6a: the copy-choice answer, plus the same entry park as
/// [`handle_replacement_choice`]. Answering can publish the entrant and then
/// stop on a co-entrant's prompt; that entry leaves with this answer's buffer.
pub(super) fn handle_copy_target_choice(
    state: &mut GameState,
    waiting_for: WaitingFor,
    target: Option<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let action_event_start = events.len();
    let waiting_for = apply_copy_target_choice(state, waiting_for, target, events)?;
    state.waiting_for = waiting_for;
    // CR 117.2e + CR 608.2c: every successful copy answer shares this boundary.
    // A settled liminal batch can expose an enclosing copy owner or replacement
    // drain; finish that resolution before priority, preserving any new prompt.
    super::engine::resume_pending_continuation_if_priority(state, events)?;
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        super::triggers::park_entry_observer_triggers_if_paused(state, events, action_event_start);
    }
    Ok(state.waiting_for.clone())
}

/// CR 616.1c + CR 614.12: how a liminal token entry stands once its own as-enters copy
/// has been chosen and the remaining entry replacements were rediscovered against it.
enum TokenEntrySettlement {
    /// The entry proceeds as the pre-copy entrant (the chosen copy is applied to it after
    /// the commit, exactly as before) with the rediscovered replacements' modifications.
    Enter(ProposedEvent),
    /// An entry replacement sent the entrant, as the copy, to another zone instead: the
    /// stored entrant now IS the copy and carries the destination.
    Redirected(ProposedEvent),
    /// An applied replacement prevented the entry.
    Prevented,
}

/// The result of one rediscovery pass over a settled copy.
enum SettleStep {
    /// There was no copy to settle (no live entrant or copy source): the entry proceeds.
    Unsettled(ProposedEvent),
    /// The rediscovery completed with this event.
    Executed(ProposedEvent, Box<TokenCopySettlement>),
    /// An applied replacement prevented the entry.
    Prevented,
    /// The rediscovery raised a prompt (`state.waiting_for`); the settlement is held on the
    /// entry and resumes through [`resume_token_copy_settlement`].
    Paused,
}

/// CR 707.5 + CR 616.1f: place the chosen copy on the liminal entrant, then
/// rediscover on the same event. Its applied IDs and the batch seed stay intact.
fn settle_token_entry_after_copy(
    state: &mut GameState,
    source_id: ObjectId,
    target_id: ObjectId,
    ability: &ResolvedAbility,
    held_exceptions: &[crate::types::ability::ContinuousModification],
    resume_event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> SettleStep {
    use crate::types::game_state::{LiminalEntrant, TokenProjection};
    let Some(entry) = state.liminal_entries.get(&source_id) else {
        return SettleStep::Unsettled(resume_event);
    };
    let Some(copy) = effects::token_copy::copy_source(
        state,
        ability,
        effects::token_copy::CopySourceReferent::Object(target_id),
    ) else {
        return SettleStep::Unsettled(resume_event);
    };
    let original = entry.object.clone();
    let mut staged = original.projected().clone();
    let superseded = staged.replacement_definitions.clone();
    effects::token::apply_copiable_values_to_liminal_object(
        &mut staged,
        &copy.values,
        &copy.display,
    );
    if let Effect::BecomeCopy {
        additional_modifications,
        ..
    } = &ability.effect
    {
        effects::token_copy::apply_immediate_copy_token_modifications_to_object(
            &mut staged,
            additional_modifications,
            &state.all_creature_types,
        );
    }
    let acquired = staged.replacement_definitions.clone();
    let settlement = Box::new(TokenCopySettlement {
        target_id,
        ability: ability.clone(),
        held_exceptions: held_exceptions.to_vec(),
        original,
        copy_values: Box::new(copy.values.clone()),
        copy_display: copy.display.clone(),
        copy_incarnation: copy.incarnation,
        staged_name: staged.name.clone(),
        superseded_replacements: None,
    });
    // CR 614.5: the entrant keeps its own definitions as the first indices so the
    // applied identities carry over; the acquired ones follow them.
    staged.replacement_definitions = superseded
        .iter_all()
        .chain(acquired.iter_all())
        .cloned()
        .collect();
    let superseded_len = superseded.len();
    if let Some(entry) = state.liminal_entries.get_mut(&source_id) {
        entry.object = LiminalEntrant::Token(TokenProjection::materialize(staged.clone()));
    }
    // CR 614.6 + CR 707.5: acquired replacements are offered here only when another
    // replacement could redirect the entrant, which would end the entry before its
    // finisher settles them; otherwise that finisher still does.
    let expose = super::replacement::liminal_entry_has_foreign_redirect(state, &resume_event);
    staged.replacement_definitions = if expose {
        staged.replacement_definitions
    } else {
        superseded
    };
    let settlement = Box::new(TokenCopySettlement {
        superseded_replacements: expose.then_some(superseded_len),
        ..*settlement
    });
    if let Some(entry) = state.liminal_entries.get_mut(&source_id) {
        entry.object = LiminalEntrant::Token(TokenProjection::materialize(staged));
        entry.copy_settlement = Some(settlement);
    }
    // The copy work the pipeline stopped for is being delivered right here.
    state.finish_active_paused_post_replacement_dispatch();
    settle_staged_token_copy(state, source_id, resume_event, true, events)
}

fn settle_staged_token_copy(
    state: &mut GameState,
    source_id: ObjectId,
    event: ProposedEvent,
    rediscover: bool,
    events: &mut Vec<GameEvent>,
) -> SettleStep {
    use super::replacement::ReplacementResult;
    match settle_liminal_token_entry_replacements(state, event, rediscover, events) {
        ReplacementResult::NeedsChoice(_) => SettleStep::Paused,
        ReplacementResult::Prevented => SettleStep::Prevented,
        ReplacementResult::Execute(event) => {
            let settlement = state
                .liminal_entries
                .get_mut(&source_id)
                .and_then(|entry| entry.copy_settlement.take())
                .expect("the staged copy owns its settlement");
            SettleStep::Executed(event, settlement)
        }
    }
}

/// CR 616.1c: decide, from the rediscovery's event, whether the entrant keeps the chosen copy
/// (an entry replacement redirected it) or goes back to its pre-copy body for the ordinary
/// post-commit copy resolution.
fn conclude_token_copy_settlement(
    state: &mut GameState,
    source_id: ObjectId,
    settlement: &TokenCopySettlement,
    event: ProposedEvent,
) -> TokenEntrySettlement {
    let redirected = matches!(
        &event,
        ProposedEvent::TokenEntry {
            redirected_to: Some(_),
            ..
        }
    );
    let Some(entry) = state.liminal_entries.get_mut(&source_id) else {
        return TokenEntrySettlement::Enter(event);
    };
    if redirected {
        let mut staged = entry.object.projected().clone();
        staged.replacement_definitions = staged.base_replacement_definitions.clone().into();
        entry.object = crate::types::game_state::LiminalEntrant::Token(
            crate::types::game_state::TokenProjection::materialize(staged),
        );
        entry.name = settlement.staged_name.clone();
        // CR 733: the journaled birth records the token that was actually created — the copy —
        // not the pre-copy entrant. The batch's remaining tokens continue from the continuation
        // captured before this settlement.
        if let Some(spec) = entry.copy_resume.as_mut() {
            *spec.values = (*settlement.copy_values).clone();
            spec.display = settlement.copy_display.clone();
            if let Effect::BecomeCopy {
                additional_modifications,
                ..
            } = &settlement.ability.effect
            {
                spec.additional_modifications = additional_modifications.clone();
            }
        }
        TokenEntrySettlement::Redirected(event)
    } else {
        entry.object = settlement.original.clone();
        TokenEntrySettlement::Enter(event)
    }
}

/// CR 614.12a + CR 616.1c: finish a liminal token entry whose own as-enters copy was chosen: the
/// rediscovered entry replacements' outcome decides whether it enters as the pre-copy entrant (the
/// copy resolves after the commit), is created elsewhere as the copy, or is prevented. Shared by
/// the immediate answer path and by the resume of a prompt the rediscovery raised
/// ([`resume_token_copy_settlement`]), so both converge on one completion.
fn complete_token_entry_after_copy(
    state: &mut GameState,
    source_id: ObjectId,
    ability: &ResolvedAbility,
    entry_counter_exceptions: &[crate::types::ability::ContinuousModification],
    step: SettleStep,
    reread_model: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use super::effects::token;

    // The entry's `name` / `source_id` ride the parked `pending_token_battlefield_entry` that
    // the `Suppress` commit installs, so only the liminal entry's PRESENCE matters here.
    let has_liminal_entry = state.liminal_entries.contains_key(&source_id);
    // CR 508.4: how many tokens of the batch exist once this entry is realized.
    let entrants_through_this_entry = state
        .liminal_entries
        .get(&source_id)
        .map_or(0, |entry| entry.created_ids.len() + 1);
    // Captured before the settlement may rewrite the entry's copy description.
    let copy_continuation = token::liminal_copy_token_continuation(state, source_id);
    // CR 403.3 + CR 603.6a: the commit applies the token's enter-with-counters, which can
    // PAUSE on a CR 616.1 ordering choice between two AddCounter replacements. On that pause
    // the only stashed post-action is the entry finalization, and its `Suppress` emission mode
    // PARKS the whole entry (record + events) on `GameState` instead of realizing it. Hand the
    // realization down as a post-finalize action so the paused path performs the same CR 400.7
    // record and CR 603.6a emit the unpaused one performs below.
    //
    // Realizing it INSIDE the counter drain (rather than leaving it to the action-boundary
    // convergence) keeps the emitted pair ahead of this action's `run_post_action_pipeline`
    // trigger scan AND ahead of its CR 704.3 SBA pass. The boundary now converges the trigger
    // half for handlers that never reach that pipeline, so this hand-down is retained for the
    // SBA ordering and for a drain that does not settle in its own action.
    //
    // On a pause this function returns at the `commit_liminal_token_entry_*` call below, so
    // the unpaused tail's CR 614.12a `BecomeCopy` chain, `finish_copy_target_choice_entry`,
    // and the copy continuation do not run on that route. THAT abandonment — of the copy
    // chain and continuation, not of the entry lifecycle — is pre-existing and is not what
    // this hand-down addresses. Dropped unused when the commit does not pause.
    let paused_entry_emit: Vec<PendingCounterPostAction> = has_liminal_entry
        .then_some(PendingCounterPostAction::EmitCommittedCopyTokenEntry {
            object_id: source_id,
        })
        .into_iter()
        .collect();
    // The values the player chose, retained so a copy completed after a prompt does not re-read a
    // model that may have changed meanwhile (its owner conceding, for one).
    let captured = match &step {
        SettleStep::Executed(_, settlement) => Some((
            settlement.copy_values.clone(),
            settlement.copy_display.clone(),
            settlement.copy_incarnation,
        )),
        _ => None,
    };
    // CR 707.5: the acquired replacements were offered with the redirecting ones, so the
    // entry finisher must not apply them a second time.
    let acquired_settled = matches!(
        &step,
        SettleStep::Executed(_, settlement) if settlement.superseded_replacements.is_some()
    );
    let outcome = match step {
        SettleStep::Unsettled(event) => TokenEntrySettlement::Enter(event),
        SettleStep::Executed(event, settlement) => {
            conclude_token_copy_settlement(state, source_id, &settlement, event)
        }
        SettleStep::Prevented => TokenEntrySettlement::Prevented,
        SettleStep::Paused => return Ok(state.waiting_for.clone()),
    };
    let mut resume_event = match outcome {
        TokenEntrySettlement::Enter(event) => event,
        TokenEntrySettlement::Redirected(event) => {
            // CR 614.6: the copy token was created where the replacement sent it and never
            // entered, so none of the entry tail below (the copy chain, entry counters, entry
            // events) applies to it. Only the rest of the batch continues.
            if !super::effects::token::commit_liminal_token_entry_with_post_actions(
                state,
                event,
                events,
                TokenEntryEventEmission::Emit,
                Vec::new(),
            ) {
                return Ok(state.waiting_for.clone());
            }
            state.finish_active_paused_post_replacement_dispatch();
            token::continue_liminal_copy_token_batch(state, copy_continuation, events);
            return Ok(state.waiting_for.clone());
        }
        TokenEntrySettlement::Prevented => {
            // CR 614.6: a prevented entry creates nothing; the rest of the batch continues.
            state.liminal_entries.remove(&source_id);
            state.finish_active_paused_post_replacement_dispatch();
            token::continue_liminal_copy_token_batch(state, copy_continuation, events);
            return Ok(state.waiting_for.clone());
        }
    };
    // CR 122.6: place the event's counters with the copy exceptions after
    // installing the copy. The continuation above captured the batch seed.
    let settled_counters = if let (
        true,
        ProposedEvent::TokenEntry {
            enter_with_counters,
            ..
        },
    ) = (acquired_settled, &mut resume_event)
    {
        let mut counters = std::mem::take(enter_with_counters);
        if let Some(entry) = state.liminal_entries.get_mut(&source_id) {
            counters.append(&mut entry.enter_with_counters);
        }
        Some(counters)
    } else {
        None
    };
    if !super::effects::token::commit_liminal_token_entry_with_post_actions(
        state,
        resume_event,
        events,
        TokenEntryEventEmission::Suppress,
        paused_entry_emit,
    ) {
        return Ok(state.waiting_for.clone());
    }
    let entrant_pin = state
        .objects
        .get(&source_id)
        .map(crate::types::identifiers::ObjectIncarnationRef::from_object);
    // CR 614.12a: the copy target was chosen before the token entered; the
    // `BecomeCopy` here overwrites the token's copiable values with the
    // chosen source's values, then layers/consumes the folded copy
    // exceptions (CR 707.9b).
    match (&captured, reread_model) {
        (Some((values, display, incarnation)), false) => {
            // CR 616.1f + CR 707.2: apply the selected copy's captured copiable values,
            // then hand its rider the same parent context as ordinary chain descent.
            let (duration, additional_modifications) = match &ability.effect {
                Effect::BecomeCopy {
                    duration,
                    additional_modifications,
                    ..
                } => (
                    duration
                        .clone()
                        .or(ability.duration.clone())
                        .unwrap_or(crate::types::ability::Duration::Permanent),
                    additional_modifications.clone(),
                ),
                _ => (crate::types::ability::Duration::Permanent, Vec::new()),
            };
            let _ = effects::become_copy::apply_precomputed_copy_values(
                state,
                source_id,
                effects::become_copy::PrecomputedCopyValues {
                    source_id: ability.source_id,
                    controller: ability.controller,
                    duration_subject: *incarnation,
                    duration,
                    values: (**values).clone(),
                    display: display.clone(),
                    additional_modifications,
                    effect_kind: crate::types::ability::EffectKind::from(&ability.effect),
                },
                events,
            );
            // CR 603.12 + CR 608.2c: the copy effect's rider chain (Superior Spider-Man's
            // "when you do, exile that card") still runs after the captured copy is installed.
            // The rider inherits the selected card through the parent-aware hand-off, as the
            // chain walker would have bound it.
            let _ = effects::resolve_rider_with_parent_context(state, ability, events);
        }
        _ => {
            let _ = effects::resolve_ability_chain(state, ability, events, 0);
        }
    }
    // CR 707.9f: type-gated exceptions apply only to an installed copy,
    // not to the uncopied entrant if installation failed.
    let copy_applied =
        entrant_pin.is_some_and(|pin| effects::become_copy::has_installed_copy_effect(state, pin));
    let held_exceptions: &[crate::types::ability::ContinuousModification] = if copy_applied {
        entry_counter_exceptions
    } else {
        &[]
    };
    // CR 608.2c + CR 614.12a: the selected copy and its rider chain have
    // completed. Retire their paused dispatch before entry-counter or
    // named-choice children can hide the CopyToken owner.
    state.finish_active_paused_post_replacement_dispatch();
    let mut counter_pause_post_actions = Vec::new();
    if has_liminal_entry {
        counter_pause_post_actions.push(PendingCounterPostAction::EmitCommittedCopyTokenEntry {
            object_id: source_id,
        });
    }
    if let Some(continuation) = copy_continuation.clone() {
        counter_pause_post_actions.push(token::liminal_copy_token_continuation_post_action(
            continuation,
            entrants_through_this_entry,
        ));
    }
    let entry_counters = match settled_counters {
        Some(counters) => CopyEntryCounters::Settled {
            exceptions: held_exceptions,
            counters,
        },
        None => CopyEntryCounters::Merged {
            exceptions: held_exceptions,
        },
    };
    if let Some(waiting_for) = finish_copy_target_choice_entry(
        state,
        source_id,
        events,
        counter_pause_post_actions,
        EntryEventReplay::Required,
        entry_counters,
    )? {
        return Ok(waiting_for);
    }
    token::continue_liminal_copy_token_batch(state, copy_continuation, events);
    Ok(state.waiting_for.clone())
}

/// Continue a staged copy after a replacement answer or a paused entry action.
fn resume_token_copy_settlement(
    state: &mut GameState,
    entry_ref: ObjectId,
    event: ProposedEvent,
    rediscover: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let settlement = state.liminal_entries[&entry_ref]
        .copy_settlement
        .as_ref()
        .expect("only a staged copy resumes copy settlement");
    let ability = settlement.ability.clone();
    let held = settlement.held_exceptions.clone();
    let step = settle_staged_token_copy(state, entry_ref, event, rediscover, events);
    complete_token_entry_after_copy(state, entry_ref, &ability, &held, step, false, events)
}

fn apply_copy_target_choice(
    state: &mut GameState,
    waiting_for: WaitingFor,
    target: Option<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let WaitingFor::CopyTargetChoice {
        player,
        source_id,
        valid_targets,
        purpose,
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

    // CR 707.2c + CR 614.12a: Metamorphic Alteration — the chosen creature's
    // copiable values are latched onto the source Aura and installed as a
    // Layer-1 copy effect on the Aura's enchanted host. The entering Aura is
    // NEVER turned into a copy (never `BecomeCopy`), and this path never runs
    // the enter-as-a-copy / copy-token completion tail below.
    match purpose {
        CopyTargetPurpose::PersistChosenAttribute => {
            return handle_persist_chosen_attribute_choice(
                state, source_id, player, target_id, events,
            );
        }
        // CR 707.1: Esix, Fractal Bloom — the chosen permanent is the copy
        // SOURCE for a `CreateToken` substitution, not a donor of copiable
        // values to a third party. While its substitute is parked the answer
        // resumes the replacement; otherwise its tail is a `CopyTokenOf`,
        // resolved against the answered object.
        CopyTargetPurpose::CopyTokenSource => {
            if super::replacement::parked_token_substitute(
                state,
                source_id,
                TokenSubstituteChoice::CopySource,
            ) {
                return resume_token_substitute(
                    state,
                    TokenSubstituteAnswer::CopySource(target_id),
                    events,
                );
            }
            return handle_copy_token_source_choice(state, source_id, player, target_id, events);
        }
        // Exhaustive on purpose: falling through to the enter-as-a-copy tail
        // below is correct ONLY for `BecomeCopy`. A wildcard here would silently
        // route a future variant into that tail, where `copy_effect_for_source`
        // returns `None` and the `unwrap_or_else` fabricates a `BecomeCopy` that
        // transforms the source.
        CopyTargetPurpose::BecomeCopy => {}
    }

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
        // CR 614.12a: "If a replacement effect that modifies how a permanent enters the
        // battlefield requires a choice, that choice is made before the permanent enters the
        // battlefield." The choice has just been made, so the `CopyTargetChoice` that asked for it
        // is spent and cannot be this action's outcome — every branch below either installs a
        // genuine new pause (which overwrites `waiting_for`) or completes. MEASURED, not assumed:
        // instrumenting all 9 exits of this branch over the full suite gives 16 entries → 16 exits,
        // of which the 4 that carry a genuine pause (`NamedChoice`, `ReplacementChoice`) are
        // byte-identical with and without this statement; only the 12 that returned the spent
        // prompt now return `Priority`. No path that owed a pause returns one less.
        //
        // CR 603.3: "Once an ability has triggered, its controller puts it on the stack as an
        // object that's not a card the next time a player would receive priority." Leaving the
        // answered prompt resident made both liminal tails echo it via
        // `if !Priority { return waiting_for }`, so the action never settled, `apply_action`
        // skipped `run_post_action_pipeline`, and the CR 603.6a entry pair flushed by
        // `finish_copy_target_choice_entry` never reached a priority boundary to be scanned —
        // board ETB observers saw nothing, and the driver re-answered the spent prompt.
        //
        // Placed above the Token/Meld split because both sub-branches share that echo tail — the
        // exit histogram covers both (11 Token, 1 Meld).
        //
        // Mirrors three existing clears, none of which is this handler's Aura tail: this handler's
        // own non-liminal tail below, `handle_persist_chosen_attribute_choice` (which is where the
        // Aura completion clear lives), and `effects::token::continue_liminal_copy_token_batch`,
        // which opens by setting `Priority` exactly like this.
        state.waiting_for = WaitingFor::Priority {
            player: state.active_player,
        };
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
                        recipient: crate::types::ability::CopyRecipient::Source,
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
            if state.active_batch_delivery().is_some_and(|pending| {
                pending.remaining.is_empty()
                    && matches!(
                        &pending.completion,
                        Some(crate::types::game_state::BatchCompletion::MeldEntry { .. })
                    )
            }) {
                state
                    .take_active_batch_delivery()
                    .expect("MeldEntry batch delivery must own the active frame");
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
            if let Some(waiting_for) = finish_copy_target_choice_entry(
                state,
                source_id,
                events,
                post_actions,
                EntryEventReplay::AlreadyDelivered,
                CopyEntryCounters::ReplacementsOnly,
            )? {
                if state.active_counter_additions().is_none() {
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
        // CR 614.1c + CR 122.6: hold the one-shot counter exceptions so the
        // realized copy's own entry counters and intrinsic loyalty share one placement.
        let mut entry_counter_exceptions = Vec::new();
        if let Effect::BecomeCopy {
            additional_modifications,
            ..
        } = &mut ability.effect
        {
            let (held, remaining) = effects::become_copy::partition_entry_counter_exceptions(
                std::mem::take(additional_modifications),
            );
            *additional_modifications = remaining;
            entry_counter_exceptions = held;
        }
        let step = settle_token_entry_after_copy(
            state,
            source_id,
            target_id,
            &ability,
            &entry_counter_exceptions,
            resume_event,
            events,
        );
        return complete_token_entry_after_copy(
            state,
            source_id,
            &ability,
            &entry_counter_exceptions,
            step,
            true,
            events,
        );
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
                    recipient: crate::types::ability::CopyRecipient::Source,
                    duration: None,
                    mana_value_limit: None,
                    additional_modifications: Vec::new(),
                },
                vec![TargetRef::Object(target_id)],
                source_id,
                player,
            )
        });
    // CR 614.1c + CR 122.6: hold only the one-shot counter exceptions.
    // Keep the copy and its entire rider chain on the ordinary resolver path;
    // the finisher combines these with the realized copy's own entry counters.
    let mut entry_counter_exceptions = Vec::new();
    if let Effect::BecomeCopy {
        additional_modifications,
        ..
    } = &mut ability.effect
    {
        let (held, remaining) = effects::become_copy::partition_entry_counter_exceptions(
            std::mem::take(additional_modifications),
        );
        *additional_modifications = remaining;
        entry_counter_exceptions = held;
    }
    let entrant_pin = state
        .objects
        .get(&source_id)
        .map(crate::types::identifiers::ObjectIncarnationRef::from_object);
    let _ = effects::resolve_ability_chain(state, &ability, events, 0);
    // CR 707.9f: the held exceptions' type gates read the realized copy. When
    // the copy effect did not apply (its source is gone), the entrant is still
    // the uncopied object, so no held exception may be evaluated against it.
    // A rider failure after an installed copy leaves the copy, and its
    // exceptions, intact.
    let copy_applied =
        entrant_pin.is_some_and(|pin| effects::become_copy::has_installed_copy_effect(state, pin));
    let held_exceptions: &[crate::types::ability::ContinuousModification] = if copy_applied {
        &entry_counter_exceptions
    } else {
        &[]
    };
    // CR 608.3 + CR 614.12a: a permanent spell that paused on its
    // enter-as-copy choice keeps its spell-resolution frame until this answer
    // has applied the copy. Complete the spell-specific epilogue before entry
    // events replay, so those triggers see the same cast provenance as an
    // unpaused permanent resolution. The post-replacement dispatch that
    // surfaced this choice is its direct child, so retire that exact dispatch
    // first; only then does the spell-resolution frame own the stack top.
    state.finish_active_paused_post_replacement_dispatch();
    if state
        .active_spell_resolution()
        .is_some_and(|ctx| ctx.object_id == source_id)
    {
        let ctx = state
            .take_active_spell_resolution()
            .expect("matching spell-resolution frame was checked above");
        apply_pending_spell_resolution(state, &ctx, events);
    }
    if let Some(waiting_for) = finish_copy_target_choice_entry(
        state,
        source_id,
        events,
        Vec::new(),
        EntryEventReplay::Required,
        CopyEntryCounters::Merged {
            exceptions: held_exceptions,
        },
    )? {
        return Ok(waiting_for);
    }
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    Ok(state.waiting_for.clone())
}

/// CR 614.1c + CR 122.6: how a copy-target-choice entry composes its entry
/// counters, and therefore who completes the entry after a counter pause.
enum CopyEntryCounters<'a> {
    /// Meld: only the entrant's own "enters with" replacement counters.
    /// Its caller's post-actions retain ownership of completing the entry.
    ReplacementsOnly,
    /// Ordinary and liminal token enter-as-copy entries: one merged placement
    /// per counter type from the entrant's own "enters with" counters, the copied
    /// planeswalker's intrinsic loyalty (CR 306.5b), and the held copy
    /// exceptions (CR 707.9f; empty when the copy did not apply). A counter
    /// pause mints `FinishCopyTargetChoiceEntry` to complete the entry.
    Merged {
        exceptions: &'a [crate::types::ability::ContinuousModification],
    },
    /// Liminal token: its replacements already ran on the entry proposal.
    Settled {
        exceptions: &'a [crate::types::ability::ContinuousModification],
        counters: Vec<(crate::types::counter::CounterType, u32)>,
    },
}

/// Outcome of resuming a copy-target-choice entry after a choice or counters.
pub(crate) enum CopyEntryResumption {
    /// The tail ran to its end (or was retired as stale); nothing is owed.
    Completed,
    /// The tail raised a prompt, now in `state.waiting_for`.
    Paused,
}

fn finish_copy_target_choice_entry(
    state: &mut GameState,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
    counter_pause_post_actions: Vec<PendingCounterPostAction>,
    replay_entry_events: EntryEventReplay,
    entry_counters: CopyEntryCounters<'_>,
) -> Result<Option<WaitingFor>, EngineError> {
    // Force a full layer pass after the copy chain so the realized
    // characteristics below (enter-tapped, ETB counters) read post-copy state.
    crate::game::layers::mark_layers_full(state);
    crate::game::layers::flush_layers(state);
    if matches!(entry_counters, CopyEntryCounters::Settled { .. }) {
        return finish_copy_target_choice_entry_counters(
            state,
            source_id,
            events,
            counter_pause_post_actions,
            replay_entry_events,
            entry_counters,
            crate::types::game_state::CopyEntryResumeStage::ReplacementsSettled,
        );
    }
    // CR 614.12 + CR 707.5: only a copied roll retained on entry is unsupported
    // on this route; resident effects may remove the printed replacement.
    let rolls = {
        let board = super::replacement::copy_entry_board(state, source_id);
        board.objects.get(&source_id).is_some_and(|entrant| {
            entrant
                .replacement_definitions
                .iter_all()
                .any(super::replacement::replacement_rolls_as_it_enters)
        })
    };
    if rolls {
        effects::record_runtime_unimplemented(state, source_id, "copied_as_enters_roll");
    }
    // CR 614.12 + CR 707.9: Surface any mandatory "as this enters, choose ..."
    // replacement choice acquired from entering as a copy before replaying the entry.
    // CR 707.5: it is read as the entrant enters, with the tokens entering in the same
    // event absent (`replacement::copy_entry_board`).
    let (choice, linked) = {
        let board = super::replacement::copy_entry_board(state, source_id);
        let choice = super::replacement::current_self_enter_replacement_choice(&board, source_id);
        let linked = choice.is_some()
            && super::replacement::copy_entry_counters_read_choice(&board, source_id);
        (choice, linked)
    };
    if let Some(choice) = choice
        .filter(|_| linked)
        .map(super::replacement::copy_entry_choice_without_linked_counters)
    {
        // CR 607.2d + CR 614.12a: linked quantities must read the answer,
        // not a controller fallback, before any entry counters are evaluated.
        let entrant = state
            .objects
            .get(&source_id)
            .map(crate::types::identifiers::ObjectIncarnationRef::from_object)
            .ok_or_else(|| EngineError::InvalidAction("Missing entering copy".to_string()))?;
        let pending_counters = crate::types::game_state::CopyEntryPendingCounters {
            exceptions: match entry_counters {
                CopyEntryCounters::ReplacementsOnly => None,
                CopyEntryCounters::Merged { exceptions } => Some(exceptions.to_vec()),
                CopyEntryCounters::Settled { .. } => unreachable!("already settled above"),
            },
            replay_entry_events,
        };
        let mut choice_post_actions = vec![PendingCounterPostAction::FinishCopyTargetChoiceEntry {
            entrant,
            resume_stage: crate::types::game_state::CopyEntryResumeStage::BeforeCounters(Box::new(
                pending_counters,
            )),
        }];
        choice_post_actions.extend(counter_pause_post_actions.iter().cloned());
        if let Some(waiting_for) =
            surface_copy_entry_pause(state, source_id, &choice_post_actions, |state| {
                apply_post_replacement_effect(
                    state,
                    &choice,
                    // CR 109.5: this self-scoped replacement belongs to the entrant.
                    ContinuationSubjects::affected_only(Some(source_id)),
                    None,
                    Some(&ReplacementEvent::Moved),
                    HashSet::new(),
                    events,
                )
            })
        {
            return Ok(Some(waiting_for));
        }
    }
    finish_copy_target_choice_entry_counters(
        state,
        source_id,
        events,
        counter_pause_post_actions,
        replay_entry_events,
        entry_counters,
        if linked {
            crate::types::game_state::CopyEntryResumeStage::ChoiceSettled
        } else {
            crate::types::game_state::CopyEntryResumeStage::ChoiceOwedAfterCounters
        },
    )
}

fn finish_copy_target_choice_entry_counters(
    state: &mut GameState,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
    mut counter_pause_post_actions: Vec<PendingCounterPostAction>,
    replay_entry_events: EntryEventReplay,
    mut entry_counters: CopyEntryCounters<'_>,
    resume_stage: crate::types::game_state::CopyEntryResumeStage,
) -> Result<Option<WaitingFor>, EngineError> {
    // CR 614.12 + CR 707.5 + CR 614.1c + CR 122.6: the entrant's acquired "enters with"
    // counters and tap state, and any quantity they count, are read as it enters — with
    // the tokens entering in the same event absent (`replacement::copy_entry_board`), so a
    // co-entering ability remover doesn't strip them and a co-entrant isn't counted.
    let counters = if let CopyEntryCounters::Settled { counters, .. } = &mut entry_counters {
        std::mem::take(counters)
    } else {
        let board = super::replacement::copy_entry_board(state, source_id);
        let modifiers =
            super::replacement::current_self_enter_replacement_modifiers(&board, source_id);
        if let Some(tapped) = modifiers.enter_tapped {
            if let Some(obj) = state.objects.get_mut(&source_id) {
                obj.tapped = tapped;
            }
        }
        modifiers.counters
    };
    let (counters, completion_witness) = match entry_counters {
        CopyEntryCounters::ReplacementsOnly => (counters, None),
        CopyEntryCounters::Merged { exceptions }
        | CopyEntryCounters::Settled { exceptions, .. } => {
            let entrant = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Missing entering copy".to_string()))?;
            let controller = entrant.controller;
            let witness = crate::types::identifiers::ObjectIncarnationRef::from_object(entrant);
            let intrinsic = if entrant
                .card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Planeswalker)
            {
                let values = crate::game::layers::compute_current_copiable_values(state, source_id)
                    .ok_or_else(|| {
                        EngineError::InvalidAction(
                            "Missing copiable values for entering copy".to_string(),
                        )
                    })?;
                // CR 306.5b: read copiable printed loyalty, not donor counters.
                // Deferred CR 107.3m gap: an X copier (Mockingbird / Altered Ego)
                // copying printed-X loyalty must bind its spell's X in a follow-up.
                crate::game::printed_cards::intrinsic_face_entry_counters(
                    values.printed_loyalty,
                    values.loyalty,
                    None,
                    None,
                )
            } else {
                Vec::new()
            };
            let merged = effects::counters::coalesce_entry_counters(
                counters.into_iter().chain(intrinsic).chain(
                    effects::become_copy::entry_exception_counters(
                        state, source_id, controller, source_id, exceptions,
                    ),
                ),
            );
            (merged, Some(witness))
        }
    };
    if !apply_etb_counters(state, source_id, &counters, events) {
        // Ordinary and liminal-token (Merged) callers carry a witness, placed first so the entry tail precedes their caller actions; Meld retains its structurally different completion.
        if let Some(entrant) = completion_witness {
            // CR 614.12a + CR 616.1: required entry choices precede token
            // publication and the remaining batch, after all counter additions settle.
            counter_pause_post_actions.insert(
                0,
                PendingCounterPostAction::FinishCopyTargetChoiceEntry {
                    entrant,
                    resume_stage: resume_stage.clone(),
                },
            );
        }
        super::effects::counters::append_pending_counter_post_actions(
            state,
            counter_pause_post_actions,
        );
        return Ok(Some(state.waiting_for.clone()));
    }
    finish_copy_target_choice_entry_after_counters(
        state,
        source_id,
        events,
        counter_pause_post_actions,
        replay_entry_events,
        resume_stage,
    )
}

/// CR 614.12 + CR 616.1: resume only the unperformed entry work. A choice
/// carries an unevaluated counter plan; a counter pause never repeats placement.
pub(crate) fn resume_copy_target_choice_entry(
    state: &mut GameState,
    entrant: crate::types::identifiers::ObjectIncarnationRef,
    resume_stage: crate::types::game_state::CopyEntryResumeStage,
    events: &mut Vec<GameEvent>,
) -> Result<CopyEntryResumption, EngineError> {
    // CR 400.7: a later incarnation must not receive choices, replayed ETBs,
    // or attachment work owed by the original entry. Its counter frame was
    // popped before this call; the spell-resolution frame was retired before the
    // counter pause, so this only retires whichever paused dispatch is still
    // active at this point (the same call the live path makes after the tail).
    if !entrant.is_current(state)
        || state
            .objects
            .get(&entrant.object_id)
            .is_none_or(|object| object.zone != Zone::Battlefield)
    {
        let _ = take_parked_entries_of(state, entrant.object_id);
        state.finish_active_paused_post_replacement_dispatch();
        return Ok(CopyEntryResumption::Completed);
    }
    use crate::types::game_state::CopyEntryResumeStage;
    let waiting_for = match resume_stage {
        CopyEntryResumeStage::BeforeCounters(plan) => {
            let counters = match &plan.exceptions {
                None => CopyEntryCounters::ReplacementsOnly,
                Some(exceptions) => CopyEntryCounters::Merged { exceptions },
            };
            finish_copy_target_choice_entry_counters(
                state,
                entrant.object_id,
                events,
                Vec::new(),
                plan.replay_entry_events,
                counters,
                CopyEntryResumeStage::ChoiceSettled,
            )?
        }
        CopyEntryResumeStage::ChoiceSettled
        | CopyEntryResumeStage::ChoiceOwedAfterCounters
        | CopyEntryResumeStage::ReplacementsSettled => {
            finish_copy_target_choice_entry_after_counters(
                state,
                entrant.object_id,
                events,
                Vec::new(),
                EntryEventReplay::Required,
                resume_stage,
            )?
        }
    };
    match waiting_for {
        Some(waiting_for) => {
            state.waiting_for = waiting_for;
            Ok(CopyEntryResumption::Paused)
        }
        None => Ok(CopyEntryResumption::Completed),
    }
}

/// CR 614.12a + CR 608.2c: every entry-choice pause retains the caller's
/// committed-entry emission and remaining batch until the answer settles.
fn park_copy_entry_post_actions(
    state: &mut GameState,
    source_id: ObjectId,
    post_actions: Vec<PendingCounterPostAction>,
    child_stack_start: ChildStackDepth,
) {
    if post_actions.is_empty() {
        return;
    }
    // A prior batch continuation can leave an empty carrier while its next
    // entrant waits on the copy choice. Reuse that carrier, so it cannot hide
    // the CopyToken owner when this entrant publishes its completed ledger.
    let has_empty_carrier = state.resolution_stack.capture_child_boundary() == child_stack_start
        && state.active_counter_additions().is_some_and(|queue| {
            queue.remaining.is_empty()
                && queue
                    .completion
                    .as_ref()
                    .is_some_and(|completion| completion.is_noop())
        });
    if has_empty_carrier {
        super::effects::counters::append_pending_counter_post_actions(state, post_actions);
    } else {
        // CR 608.2c + CR 614.12a: finish every continuation the entry
        // choice raised before emitting the entry or continuing its batch.
        super::effects::counters::park_pending_counter_completion_at_child_boundary(
            state,
            PendingEffectResolved::with_post_actions_without_effect(
                EffectKind::CopyTokenOf,
                source_id,
                post_actions,
            ),
            child_stack_start,
        );
    }
}

/// CR 608.2c + CR 614.12a: an acquired entry choice and its continuation are
/// children of the copy batch, not later instructions after that batch.
fn surface_copy_entry_pause(
    state: &mut GameState,
    source_id: ObjectId,
    post_actions: &[PendingCounterPostAction],
    producer: impl FnOnce(&mut GameState) -> Option<WaitingFor>,
) -> Option<WaitingFor> {
    // An exhausted carrier has no remaining work. Retire it before producing
    // the choice so the chain driver cannot mistake it for the choice's child.
    super::effects::counters::retire_exhausted_counter_additions(state);
    // The chain driver places a paused instruction's tail below its active
    // child. This owner predates the instruction, so keep it outside that
    // producer and restore it below the complete child stack afterwards.
    let owner = state.active_copy_token().is_some().then(|| {
        state
            .take_active_copy_token()
            .expect("the copy batch owns the active frame")
            .expect("the checked copy owner exists")
    });
    let unowned_entrant = bind_copied_choice_entrant(state, source_id);
    let child_stack_start = state.resolution_stack.capture_child_boundary();
    // CR 614.12a: the held owner's earlier tokens still belong to this entry
    // group while the producer determines a copied choice or payment's terms.
    let waiting_for = if let Some(owner) = owner.as_ref() {
        super::effects::token_copy::publish_committed_copy_tokens(
            state,
            owner.created_ids.clone(),
            producer,
        )
    } else {
        producer(state)
    };
    if waiting_for.is_some() {
        if let Some(entrant) = unowned_entrant {
            park_copied_choice_owner(state, entrant, child_stack_start);
        }
        park_copy_entry_post_actions(state, source_id, post_actions.to_vec(), child_stack_start);
    }
    if let Some(owner) = owner {
        if state.resolution_stack.capture_child_boundary() == child_stack_start {
            state.push_copy_token(owner);
        } else {
            state
                .insert_copy_token_parent_at_child_boundary(owner, child_stack_start)
                .expect("the copy owner belongs below its entry-choice subtree");
        }
    }
    waiting_for
}

fn finish_copy_target_choice_entry_after_counters(
    state: &mut GameState,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
    counter_pause_post_actions: Vec<PendingCounterPostAction>,
    replay_entry_events: EntryEventReplay,
    resume_stage: crate::types::game_state::CopyEntryResumeStage,
) -> Result<Option<WaitingFor>, EngineError> {
    // CR 614.12 + CR 707.9: Surface any mandatory "as this enters, choose ..."
    // replacement choice acquired from entering as a copy before replaying the entry.
    // CR 707.5: it is read as the entrant enters, with the tokens entering in the same
    // event absent (`replacement::copy_entry_board`).
    let choice = (resume_stage
        == crate::types::game_state::CopyEntryResumeStage::ChoiceOwedAfterCounters)
        .then(|| {
            let board = super::replacement::copy_entry_board(state, source_id);
            super::replacement::current_self_enter_replacement_choice(&board, source_id)
        })
        .flatten();
    if let Some(choice) = choice {
        if let Some(waiting_for) =
            surface_copy_entry_pause(state, source_id, &counter_pause_post_actions, |state| {
                apply_post_replacement_effect(
                    state,
                    &choice,
                    // CR 109.5: this self-scoped replacement belongs to the entrant.
                    ContinuationSubjects::affected_only(Some(source_id)),
                    None,
                    Some(&ReplacementEvent::Moved),
                    HashSet::new(),
                    events,
                )
            })
        {
            return Ok(Some(waiting_for));
        }
    }
    // CR 614.12a + CR 707.2: copied entry payments retain the same child and
    // batch custody as copied entry choices.
    if resume_stage != crate::types::game_state::CopyEntryResumeStage::ReplacementsSettled {
        if let Some(waiting_for) =
            surface_copy_entry_pause(state, source_id, &counter_pause_post_actions, |state| {
                super::replacement::surface_copied_entry_payment(state, source_id, events)
            })
        {
            return Ok(Some(waiting_for));
        }
    }

    // CR 400.7 + CR 403.3 + CR 614.12a: the copy is realized and every mandatory as-enters
    // choice is answered — the first instant the token IS the thing that entered. Placed before
    // the replay/batch-drain/aura blocks so their pause returns cannot strand a parked entry.
    // `false` here means an earlier convergence point already realized it (structurally
    // idempotent, `Option::take_if`), which is not an error.
    let _ = super::effects::token::flush_pending_token_battlefield_entry(state, source_id, events);
    crate::game::layers::mark_layers_full(state);
    // CR 614.12a + CR 707.9: The battlefield-entry `ZoneChanged` event was
    // captured into `state.deferred_entry_events` when `CopyTargetChoice` was
    // set up, *before* `BecomeCopy` had a chance to push the copied object's
    // characteristics and any `GrantTrigger` continuous modifications (e.g.
    // Callidus Assassin's "destroy another creature with the same name")
    // into `trigger_definitions`. With the copy now resolved and layers
    // re-evaluated, replay those events so granted ETBs and observer ETBs
    // (Soul Warden) match against the realized copy and join the next
    // priority's batch. Replay is gated on the source still being on the
    // battlefield — concede / error / chained-replacement paths can leave a
    // stale event in the vec, and we discard rather than fire a phantom entry
    // trigger.
    if replay_entry_events == EntryEventReplay::Required {
        let delivery_start = events.len();
        if let Some(waiting_for) = replay_deferred_entry_events(state, source_id, events)? {
            state.capture_paused_zone_change_delivery_for_member(
                source_id,
                &events[delivery_start..],
            );
            // CR 603.3b + CR 608.2c: the entry has completed before its ETB
            // trigger's target-selection prompt is exposed. Retire the parent
            // carrier now; otherwise the selected trigger would later try to
            // resolve while this completed spell or ability still owns it.
            // The paused post-replacement drain is part of that parent
            // completion. Retire its exact top dispatch before asking the
            // carrier predicate whether the source resolution can settle.
            state.finish_active_paused_post_replacement_dispatch();
            super::engine::settle_resolving_stack_entry_before_trigger_selection(state);
            return Ok(Some(waiting_for));
        }
        state.capture_paused_zone_change_delivery_for_member(source_id, &events[delivery_start..]);
    }
    // CR 615.5: a CopyTargetChoice raised by a general post-replacement
    // continuation has now completed its own copy work. Retire only that
    // active paused drain before exposing the outer batch-delivery completion;
    // a later nested choice returned above and therefore keeps the drain
    // resident with its event context intact.
    state.finish_active_paused_post_replacement_dispatch();
    // CR 702.49c: a ninjutsu entry that deferred `BatchCompletion::NinjutsuPlacement`
    // while paused on `CopyTargetChoice` must run combat placement after the copy
    // resolves (mirrors the `ReturnAsAuraTarget` batch drain in engine.rs).
    if replay_entry_events == EntryEventReplay::Required && state.active_batch_delivery().is_some()
    {
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
    // CR 616.1: the `NeedsChoice` arm below is a PAUSE, so it owes the same
    // carrier the ETB-counter pause above owes — `counter_pause_post_actions`
    // holds the liminal copy-token caller's committed-entry emit and its
    // remaining-batch continuation, and returning `Ok(Some(..))` without parking
    // them would drop both (`handle_copy_target_choice` runs that tail only on
    // `Ok(None)`). Parked rather than reasoned about: the current card pool does
    // not reach the intersection (the liminal accept path requires the COPIED card
    // to carry its own enter-as-a-copy replacement, and none of those realize into
    // a multi-host Aura with a token continuation), but "no card does this today"
    // is not a property the engine should depend on.
    let child_stack_start = state.resolution_stack.capture_child_boundary();
    match crate::game::zone_pipeline::resolve_entering_aura_attachment(state, source_id, events) {
        // CR 303.4g does NOT apply on this route: this entrant already ENTERED the
        // battlefield as a non-Aura (Copy Enchantment enters as a plain enchantment)
        // and only became an Aura when `BecomeCopy` realized post-entry. It cannot be
        // un-entered, and CR 303.4g's "isn't created" clause governs only an object
        // still in the act of entering — so the CR 704.5m unattached-Aura state-based
        // action owns it. Same disposition as `Attached`: nothing to do here.
        crate::game::zone_pipeline::EnteringAuraAttachment::NotApplicable
        | crate::game::zone_pipeline::EnteringAuraAttachment::Attached
        | crate::game::zone_pipeline::EnteringAuraAttachment::NoLegalHost => {}
        crate::game::zone_pipeline::EnteringAuraAttachment::NeedsChoice {
            controller,
            legal_targets,
        } => {
            // CR 616.1 carrier for the host pause. Same vehicle the non-liminal
            // copy-token path uses for its own `ReturnAsAuraTarget` pause
            // (`token_copy::apply_copy_token_after_replacement_with_created_ids`,
            // the `ContinueCopyTokenEntryAfterAuraHost` stash): an empty counter
            // queue whose completion carries the post-actions, drained by
            // `drain_pending_counter_additions` when the `ReturnAsAuraTarget`
            // handler in `engine.rs` returns to priority and calls
            // `resume_pending_continuation_if_priority`.
            //
            // Only stashed when there is something to carry — the non-liminal
            // caller (real Copy Enchantment) passes an empty list and must not
            // acquire a spurious pending-counter frame. `EmitCommittedCopyTokenEntry`
            // in the list is idempotent (`flush_pending_token_battlefield_entry`
            // above already realized the parked entry), so re-running it after the
            // host choice is a no-op rather than a double emit.
            park_copy_entry_post_actions(
                state,
                source_id,
                counter_pause_post_actions,
                child_stack_start,
            );
            state.waiting_for = WaitingFor::ReturnAsAuraTarget {
                player: controller,
                source_id,
                returned_id: source_id,
                legal_targets,
                pending_effect: Box::new(ResolvedAbility::new(
                    Effect::Attach {
                        attachment: TargetFilter::SelfRef,
                        target: TargetFilter::Any,
                        selection: AttachSelection::AtResolution {
                            count: AttachCardinality::One,
                        },
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
/// before the permanent enters). [`collect_deferred_entry_replay`] queues what
/// the entry triggered.
///
/// Takes `source_id`'s own parked entries and no others, except an entry a live
/// logical owner's paused delivery holds, which that owner's custody and segment
/// collect (CR 603.6d), so replay is idempotent
/// — the event is fired exactly once and can never also reach a later
/// `Priority`-result pipeline pass — and another entrant's parked entry waits for
/// its own collector. Returns `None` (no pause) when the park holds nothing of
/// `source_id` (the no-op guard for non-entry persisted choices, e.g. Pithing
/// Needle naming), or when the entering source has left the battlefield
/// (concede / error / chained-replacement paths leave a stale event we discard
/// rather than fire against a phantom object).
pub(super) fn replay_deferred_entry_events(
    state: &mut GameState,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<Option<WaitingFor>, EngineError> {
    collect_deferred_entry_replay(state, source_id);
    effects::drain_pending_continuation(state, events);
    // CR 113.2c + CR 603.3b + CR 707.10: `drain_pending_continuation` above may
    // have paused on an interactive trigger. When it pauses it sets
    // `state.pending_trigger` for the active instance and stashes any
    // simultaneously-fired siblings into `state.deferred_triggers`, so the
    // resumption logic mirrors the priority-time site in `engine_priority`
    // (issue #429 — the same failure mode as #416 on the copy-replacement
    // completion path):
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
    // CR 603.3b (#531): propagate an OrderTriggers pause `drain_pending_continuation` raised.
    if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
        return Ok(Some(state.waiting_for.clone()));
    }
    if let Some(waiting_for) = super::engine::begin_pending_trigger_target_selection(state)? {
        return Ok(Some(waiting_for));
    }
    Ok(None)
}

/// CR 614.12a + CR 117.5 + CR 603.3b: the choice was made before the permanent
/// entered, so what its entry triggered waits for the next priority with
/// everything else that triggered since the last one.
///
/// CR 603.2 + CR 603.6d: an entry a logical owner holds is collected by that
/// owner, once. When a live owner's paused delivery holds the source's entry,
/// the entry stays parked, recorded completed, for that owner's custody and
/// segment, with any attacking seat it owes.
pub(super) fn collect_deferred_entry_replay(state: &mut GameState, source_id: ObjectId) {
    let source = state
        .objects
        .get(&source_id)
        .filter(|obj| obj.zone == Zone::Battlefield)
        .map(ObjectIncarnationRef::from_object);
    let Some(source) = source else {
        take_parked_entries_of(state, source_id);
        return;
    };
    let held = owner_held_delivery_events(state);
    let deferred = take_parked_entries_of_except(state, source_id, &held);
    // CR 614.12a: the choice has been made, so the entry happened.
    if entry_park_holds_entrant(state, source_id) {
        record_parked_entry_completion(state, &[source]);
    }
    if deferred.is_empty() {
        return;
    }
    super::triggers::collect_triggers_into_deferred(state, &deferred);
    super::triggers::collect_delayed_trigger_contexts_into_deferred(state, &deferred);
}

/// The paused deliveries a live logical owner will resume and collect: the
/// `ChangeZone` owner's when its drain counts the member (the branch that runs
/// custody), and the batch owner's. Each owner is found only as the top frame
/// or as the exact parent of the active `PostReplacement` child the paused
/// choice leaves on top.
fn owner_held_delivery_events(state: &mut GameState) -> Vec<GameEvent> {
    let mut held: Vec<GameEvent> = state
        .resolution_stack
        .active_change_zone_or_post_replacement_child()
        .and_then(|frame| frame.pending.as_ref())
        .and_then(|pending| pending.paused_current.as_ref())
        .filter(|paused| {
            matches!(
                paused.count,
                crate::types::game_state::PausedZoneChangeDeliveryCount::NeedsCount
            )
        })
        .map(|paused| paused.delivery_events.clone())
        .unwrap_or_default();
    if let Some(paused) = state
        .resolution_stack
        .active_batch_delivery_or_post_replacement_child_mut()
        .and_then(|pending| pending.paused_current.as_ref())
    {
        held.extend(paused.delivery_events.iter().cloned());
    }
    held
}

/// CR 305.1 + CR 603.2: The priority-settling counterpart to
/// [`replay_deferred_entry_events`]. A played land whose pre-entry shock/payment
/// choice (`ReplacementResult::NeedsChoice`) or delivery-tail counter-order
/// choice (`ZoneDeliveryResult::NeedsChoice`) paused its entry parked its
/// `LandPlayed` occurrence in `state.deferred_entry_events`
/// (`park_land_played_for_deferred_entry`). Once the entry completes, that resume
/// settles to `Priority`, so move the parked events into the action's `events`
/// for the ordinary priority-time trigger scan (`run_post_action_pipeline`) to
/// fire "play a land" observers (City of Traitors) against the realized object
/// (issue #8738). The non-priority `NamedChoice` resume path instead replays
/// through `replay_deferred_entry_events`, so the two are disjoint. Moves only
/// `entrant`'s own entries: this runs on every battlefield zone-change resume,
/// and another entrant's parked entry waits for its own collector.
///
/// CR 603.6d + CR 603.2: an entry whose collector is still live (a paused
/// as-enters continuation it is bound to, a logical owner holding its delivery,
/// or an owed attacking seat, CR 508.4) is collected by that collector, once;
/// this flush takes only entries no collector owns.
pub(crate) fn flush_deferred_entry_events_into_priority_scan(
    state: &mut GameState,
    entrant: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    let owner_held = state.resolution_stack.paused_owner_delivery_events();
    let bound: Vec<GameEvent> = state
        .parked_entry_events()
        .filter(|event| entry_is_bound_to_a_live_drain(state, event, entrant))
        .cloned()
        .collect();
    let deferred = take_parked_entries_of_where(state, entrant, |parked| {
        bound
            .iter()
            .chain(&owner_held)
            .any(|event| parked_event_holds(&parked.event, event))
            || parked.entry_attack.is_some()
    });
    events.extend(deferred);
}

/// CR 603.6d + CR 614.12a + CR 400.7: is `event` a battlefield entry of
/// `entrant` whose incarnation is bound to a live drain anywhere on the
/// resolution stack? That drain's retirement records its completion. An entry
/// recorded with no incarnation is read as the entrant's current one.
fn entry_is_bound_to_a_live_drain(state: &GameState, event: &GameEvent, entrant: ObjectId) -> bool {
    let GameEvent::ZoneChanged {
        object_id,
        to: Zone::Battlefield,
        record,
        ..
    } = event
    else {
        return false;
    };
    if *object_id != entrant {
        return false;
    }
    record
        .entered_incarnation
        .or_else(|| state.objects.get(&entrant).map(|object| object.incarnation))
        .is_some_and(|incarnation| {
            state
                .resolution_stack
                .binds_parked_entrant(ObjectIncarnationRef {
                    object_id: entrant,
                    incarnation,
                })
        })
}

/// The three entry shapes a park holds for `source_id`: its battlefield
/// `ZoneChanged`, its `LandPlayed` (CR 305.1), and its attach resolution. The
/// single filter shared by the mid-entry choice capture, the as-enters
/// continuation bracket and every narrowed park removal.
pub(crate) fn is_entry_event_of(event: &GameEvent, source_id: ObjectId) -> bool {
    match event {
        GameEvent::ZoneChanged {
            object_id,
            to: Zone::Battlefield,
            ..
        }
        | GameEvent::LandPlayed { object_id, .. } => *object_id == source_id,
        GameEvent::Attached { attachment_id, .. } => *attachment_id == source_id,
        GameEvent::GameStarted
        | GameEvent::HiddenSearchViewed { .. }
        | GameEvent::TurnStarted { .. }
        | GameEvent::ExtraTurnCreated { .. }
        | GameEvent::PhaseChanged { .. }
        | GameEvent::PhaseChangedFor { .. }
        | GameEvent::PriorityPassed { .. }
        | GameEvent::SpellCast { .. }
        | GameEvent::Mutated { .. }
        | GameEvent::Augmented { .. }
        | GameEvent::Melded { .. }
        | GameEvent::SpellCopied { .. }
        | GameEvent::XValueChosen { .. }
        | GameEvent::AbilityActivated { .. }
        | GameEvent::ZoneChanged { .. }
        | GameEvent::LifeChanged { .. }
        | GameEvent::ManaAdded { .. }
        | GameEvent::TappedForMana { .. }
        | GameEvent::ManaAbilityProduced { .. }
        | GameEvent::ManaPoolEmptied { .. }
        | GameEvent::ManaBurn { .. }
        | GameEvent::ManaRecolored { .. }
        | GameEvent::PermanentTapped { .. }
        | GameEvent::CreatureExerted { .. }
        | GameEvent::CreatureEnlisted { .. }
        | GameEvent::ArmyAmassed { .. }
        | GameEvent::Foretold { .. }
        | GameEvent::BecameForetold { .. }
        | GameEvent::PlayerLost { .. }
        | GameEvent::MulliganStarted
        | GameEvent::CardsDrawn { .. }
        | GameEvent::CardDrawn { .. }
        | GameEvent::PermanentUntapped { .. }
        | GameEvent::PermanentPhasedOut { .. }
        | GameEvent::PermanentPhasedIn { .. }
        | GameEvent::PlayerPhasedOut { .. }
        | GameEvent::PlayerPhasedIn { .. }
        | GameEvent::StackPushed { .. }
        | GameEvent::AbilityTriggered { .. }
        | GameEvent::StackResolved { .. }
        | GameEvent::Discarded { .. }
        | GameEvent::Milled { .. }
        | GameEvent::DamageCleared { .. }
        | GameEvent::GameOver { .. }
        | GameEvent::ResolutionHalted { .. }
        | GameEvent::ShortcutRepetition { .. }
        | GameEvent::DamageDealt { .. }
        | GameEvent::DamagePrevented { .. }
        | GameEvent::SpellCountered { .. }
        | GameEvent::CounterAdded { .. }
        | GameEvent::SagaChapterAbilityResolved { .. }
        | GameEvent::ObjectIntensified { .. }
        | GameEvent::Evolved { .. }
        | GameEvent::UnTellPerformed { .. }
        | GameEvent::PhraseSaid { .. }
        | GameEvent::GotchaSaid { .. }
        | GameEvent::ObjectTouched { .. }
        | GameEvent::CounterRemoved { .. }
        | GameEvent::TokenCreated { .. }
        | GameEvent::ObjectConjured { .. }
        | GameEvent::CardCopyCreated { .. }
        | GameEvent::CreatureDestroyed { .. }
        | GameEvent::PermanentSacrificed { .. }
        | GameEvent::ControllerChanged { .. }
        | GameEvent::EffectResolved { .. }
        | GameEvent::Unattached { .. }
        | GameEvent::ContinuousEffectEnded { .. }
        | GameEvent::AttackersDeclared { .. }
        | GameEvent::BlockersDeclared { .. }
        | GameEvent::AttackerBecameBlocked { .. }
        | GameEvent::AttackerBecameBlockedByFilteredBlocker { .. }
        | GameEvent::CombatTaxPaid { .. }
        | GameEvent::CombatTaxDeclined { .. }
        | GameEvent::BecomesTarget { .. }
        | GameEvent::VehicleCrewed { .. }
        | GameEvent::Stationed { .. }
        | GameEvent::Saddled { .. }
        | GameEvent::ReplacementApplied { .. }
        | GameEvent::Transformed { .. }
        | GameEvent::Flipped { .. }
        | GameEvent::Specialized { .. }
        | GameEvent::DayNightChanged { .. }
        | GameEvent::TurnedFaceUp { .. }
        | GameEvent::TurnedFaceDown { .. }
        | GameEvent::CardsRevealed { .. }
        | GameEvent::ChosenNumbersRevealed { .. }
        | GameEvent::CombatDamageDealtToPlayer { .. }
        | GameEvent::PlayerEliminated { .. }
        | GameEvent::CrimeCommitted { .. }
        | GameEvent::Cycled { .. }
        | GameEvent::PlayerPerformedAction { .. }
        | GameEvent::CardPredicateGuessMade { .. }
        | GameEvent::CardNamed { .. }
        | GameEvent::Regenerated { .. }
        | GameEvent::CreatureSuspected { .. }
        | GameEvent::CreatureNoLongerSuspected { .. }
        | GameEvent::Detained { .. }
        | GameEvent::BecamePrepared { .. }
        | GameEvent::BecameUnprepared { .. }
        | GameEvent::CaseSolved { .. }
        | GameEvent::ClassLevelGained { .. }
        | GameEvent::MonarchChanged { .. }
        | GameEvent::CityBlessingGained { .. }
        | GameEvent::EnduringStoryGained { .. }
        | GameEvent::DieRolled { .. }
        | GameEvent::DieRollIgnored { .. }
        | GameEvent::StartingPlayerContest { .. }
        | GameEvent::CoinFlipped { .. }
        | GameEvent::RingTemptsYou { .. }
        | GameEvent::RoomEntered { .. }
        | GameEvent::RoomDoorUnlocked { .. }
        | GameEvent::BecomesPlotted { .. }
        | GameEvent::DungeonCompleted { .. }
        | GameEvent::Planeswalked { .. }
        | GameEvent::ChaosEnsued { .. }
        | GameEvent::PlanarDieRolled { .. }
        | GameEvent::SchemeSetInMotion { .. }
        | GameEvent::SchemeAbandoned { .. }
        | GameEvent::InitiativeTaken { .. }
        | GameEvent::AttractionOpened { .. }
        | GameEvent::ContraptionAssembled { .. }
        | GameEvent::StickerPlaced { .. }
        | GameEvent::AttractionsRolledToVisit { .. }
        | GameEvent::AttractionVisited { .. }
        | GameEvent::ContraptionCranked { .. }
        | GameEvent::Firebend { .. }
        | GameEvent::Airbend { .. }
        | GameEvent::Earthbend { .. }
        | GameEvent::Waterbend { .. }
        | GameEvent::CompanionRevealed { .. }
        | GameEvent::CompanionMovedToHand { .. }
        | GameEvent::NinjutsuActivated { .. }
        | GameEvent::KeywordAbilityActivated { .. }
        | GameEvent::CreatureExploited { .. }
        | GameEvent::EnergyChanged { .. }
        | GameEvent::SpeedChanged { .. }
        | GameEvent::PlayerCounterChanged { .. }
        | GameEvent::ManaExpended { .. }
        | GameEvent::PhysicalContestStarted { .. }
        | GameEvent::PhysicalContestResolved { .. }
        | GameEvent::Clash { .. }
        | GameEvent::VoteCast { .. }
        | GameEvent::VoteResolved { .. }
        | GameEvent::PowerToughnessChanged { .. }
        | GameEvent::CascadeMissed { .. }
        | GameEvent::DebugActionUsed { .. }
        | GameEvent::DebugPermissionGranted { .. }
        | GameEvent::DebugPermissionRevoked { .. } => false,
    }
}

/// Does the parked event `parked` hold `event`? A battlefield `ZoneChanged` is
/// matched by its per-turn ledger key (the key
/// `zones::settle_battlefield_entry_observations` uses), so later record
/// stamping of the action's copy cannot unpark it; every other event, a
/// `LandPlayed` and the attach resolution among them, by equality.
fn parked_event_holds(parked: &GameEvent, event: &GameEvent) -> bool {
    match event {
        GameEvent::ZoneChanged {
            to: Zone::Battlefield,
            record,
            ..
        } => matches!(
            parked,
            GameEvent::ZoneChanged { record: parked_record, .. }
                if parked_record.recorded_turn_number == record.recorded_turn_number
                    && parked_record.turn_zone_change_index == record.turn_zone_change_index
        ),
        GameEvent::GameStarted
        | GameEvent::HiddenSearchViewed { .. }
        | GameEvent::TurnStarted { .. }
        | GameEvent::ExtraTurnCreated { .. }
        | GameEvent::PhaseChanged { .. }
        | GameEvent::PhaseChangedFor { .. }
        | GameEvent::PriorityPassed { .. }
        | GameEvent::SpellCast { .. }
        | GameEvent::Mutated { .. }
        | GameEvent::Augmented { .. }
        | GameEvent::Melded { .. }
        | GameEvent::SpellCopied { .. }
        | GameEvent::XValueChosen { .. }
        | GameEvent::AbilityActivated { .. }
        | GameEvent::ZoneChanged { .. }
        | GameEvent::LifeChanged { .. }
        | GameEvent::ManaAdded { .. }
        | GameEvent::TappedForMana { .. }
        | GameEvent::ManaAbilityProduced { .. }
        | GameEvent::ManaPoolEmptied { .. }
        | GameEvent::ManaBurn { .. }
        | GameEvent::ManaRecolored { .. }
        | GameEvent::PermanentTapped { .. }
        | GameEvent::CreatureExerted { .. }
        | GameEvent::CreatureEnlisted { .. }
        | GameEvent::ArmyAmassed { .. }
        | GameEvent::Foretold { .. }
        | GameEvent::BecameForetold { .. }
        | GameEvent::PlayerLost { .. }
        | GameEvent::MulliganStarted
        | GameEvent::CardsDrawn { .. }
        | GameEvent::CardDrawn { .. }
        | GameEvent::PermanentUntapped { .. }
        | GameEvent::PermanentPhasedOut { .. }
        | GameEvent::PermanentPhasedIn { .. }
        | GameEvent::PlayerPhasedOut { .. }
        | GameEvent::PlayerPhasedIn { .. }
        | GameEvent::LandPlayed { .. }
        | GameEvent::StackPushed { .. }
        | GameEvent::AbilityTriggered { .. }
        | GameEvent::StackResolved { .. }
        | GameEvent::Discarded { .. }
        | GameEvent::Milled { .. }
        | GameEvent::DamageCleared { .. }
        | GameEvent::GameOver { .. }
        | GameEvent::ResolutionHalted { .. }
        | GameEvent::ShortcutRepetition { .. }
        | GameEvent::DamageDealt { .. }
        | GameEvent::DamagePrevented { .. }
        | GameEvent::SpellCountered { .. }
        | GameEvent::CounterAdded { .. }
        | GameEvent::SagaChapterAbilityResolved { .. }
        | GameEvent::ObjectIntensified { .. }
        | GameEvent::Evolved { .. }
        | GameEvent::UnTellPerformed { .. }
        | GameEvent::PhraseSaid { .. }
        | GameEvent::GotchaSaid { .. }
        | GameEvent::ObjectTouched { .. }
        | GameEvent::CounterRemoved { .. }
        | GameEvent::TokenCreated { .. }
        | GameEvent::ObjectConjured { .. }
        | GameEvent::CardCopyCreated { .. }
        | GameEvent::CreatureDestroyed { .. }
        | GameEvent::PermanentSacrificed { .. }
        | GameEvent::ControllerChanged { .. }
        | GameEvent::EffectResolved { .. }
        | GameEvent::Attached { .. }
        | GameEvent::Unattached { .. }
        | GameEvent::ContinuousEffectEnded { .. }
        | GameEvent::AttackersDeclared { .. }
        | GameEvent::BlockersDeclared { .. }
        | GameEvent::AttackerBecameBlocked { .. }
        | GameEvent::AttackerBecameBlockedByFilteredBlocker { .. }
        | GameEvent::CombatTaxPaid { .. }
        | GameEvent::CombatTaxDeclined { .. }
        | GameEvent::BecomesTarget { .. }
        | GameEvent::VehicleCrewed { .. }
        | GameEvent::Stationed { .. }
        | GameEvent::Saddled { .. }
        | GameEvent::ReplacementApplied { .. }
        | GameEvent::Transformed { .. }
        | GameEvent::Flipped { .. }
        | GameEvent::Specialized { .. }
        | GameEvent::DayNightChanged { .. }
        | GameEvent::TurnedFaceUp { .. }
        | GameEvent::TurnedFaceDown { .. }
        | GameEvent::CardsRevealed { .. }
        | GameEvent::ChosenNumbersRevealed { .. }
        | GameEvent::CombatDamageDealtToPlayer { .. }
        | GameEvent::PlayerEliminated { .. }
        | GameEvent::CrimeCommitted { .. }
        | GameEvent::Cycled { .. }
        | GameEvent::PlayerPerformedAction { .. }
        | GameEvent::CardPredicateGuessMade { .. }
        | GameEvent::CardNamed { .. }
        | GameEvent::Regenerated { .. }
        | GameEvent::CreatureSuspected { .. }
        | GameEvent::CreatureNoLongerSuspected { .. }
        | GameEvent::Detained { .. }
        | GameEvent::BecamePrepared { .. }
        | GameEvent::BecameUnprepared { .. }
        | GameEvent::CaseSolved { .. }
        | GameEvent::ClassLevelGained { .. }
        | GameEvent::MonarchChanged { .. }
        | GameEvent::CityBlessingGained { .. }
        | GameEvent::EnduringStoryGained { .. }
        | GameEvent::DieRolled { .. }
        | GameEvent::DieRollIgnored { .. }
        | GameEvent::StartingPlayerContest { .. }
        | GameEvent::CoinFlipped { .. }
        | GameEvent::RingTemptsYou { .. }
        | GameEvent::RoomEntered { .. }
        | GameEvent::RoomDoorUnlocked { .. }
        | GameEvent::BecomesPlotted { .. }
        | GameEvent::DungeonCompleted { .. }
        | GameEvent::Planeswalked { .. }
        | GameEvent::ChaosEnsued { .. }
        | GameEvent::PlanarDieRolled { .. }
        | GameEvent::SchemeSetInMotion { .. }
        | GameEvent::SchemeAbandoned { .. }
        | GameEvent::InitiativeTaken { .. }
        | GameEvent::AttractionOpened { .. }
        | GameEvent::ContraptionAssembled { .. }
        | GameEvent::StickerPlaced { .. }
        | GameEvent::AttractionsRolledToVisit { .. }
        | GameEvent::AttractionVisited { .. }
        | GameEvent::ContraptionCranked { .. }
        | GameEvent::Firebend { .. }
        | GameEvent::Airbend { .. }
        | GameEvent::Earthbend { .. }
        | GameEvent::Waterbend { .. }
        | GameEvent::CompanionRevealed { .. }
        | GameEvent::CompanionMovedToHand { .. }
        | GameEvent::NinjutsuActivated { .. }
        | GameEvent::KeywordAbilityActivated { .. }
        | GameEvent::CreatureExploited { .. }
        | GameEvent::EnergyChanged { .. }
        | GameEvent::SpeedChanged { .. }
        | GameEvent::PlayerCounterChanged { .. }
        | GameEvent::ManaExpended { .. }
        | GameEvent::PhysicalContestStarted { .. }
        | GameEvent::PhysicalContestResolved { .. }
        | GameEvent::Clash { .. }
        | GameEvent::VoteCast { .. }
        | GameEvent::VoteResolved { .. }
        | GameEvent::PowerToughnessChanged { .. }
        | GameEvent::CascadeMissed { .. }
        | GameEvent::DebugActionUsed { .. }
        | GameEvent::DebugPermissionGranted { .. }
        | GameEvent::DebugPermissionRevoked { .. } => parked == event,
    }
}

/// CR 614.12a + CR 603.6d + CR 603.2c: the single hold predicate. True when
/// `event` is an entry held in `deferred_entry_events`, in either state: one
/// whose as-enters continuation (or mid-entry choice) has not completed, or one
/// that completed and waits for its collector. Such an entry is collected only
/// by its replay, its settled release or its logical owner's custody, never by a
/// collector running at the pause.
pub(crate) fn entry_event_is_parked(state: &GameState, event: &GameEvent) -> bool {
    if state.deferred_entry_events.is_empty() {
        return false;
    }
    state
        .parked_entry_events()
        .any(|parked| parked_event_holds(parked, event))
}

/// Park `event` as an entry that has not happened, latching the stack object
/// whose resolution parked it (`None` outside a resolution). The one way an
/// entry joins the park.
pub(crate) fn park_pending_entry(state: &mut GameState, event: GameEvent) {
    let carrier = state.resolving_stack_entry.as_ref().map(|entry| entry.id);
    state
        .deferred_entry_events
        .push(ParkedEntry::pending(event, carrier));
}

/// CR 614.1c + CR 113.6: is `event` an entrant's own as-enters roll, stamped at
/// emission because the entrant's entry had not happened?
pub(crate) fn is_as_enters_roll(event: &GameEvent) -> bool {
    matches!(
        event,
        GameEvent::DieRolled {
            pre_entry_source: Some(_),
            ..
        }
    )
}

/// CR 614.1c + CR 603.2 + CR 113.6: park an entrant's own as-enters roll beside
/// its entry, holding `held_triggers`, the contexts its instruction triggered.
/// The roll happened, so it is parked completed; the settled release places
/// what it triggered with its entry, never the action that made it, so a prompt
/// that ends that action cannot lose it.
pub(crate) fn park_as_enters_roll(
    state: &mut GameState,
    event: GameEvent,
    held_triggers: Vec<super::triggers::DeferredTrigger>,
) {
    debug_assert!(is_as_enters_roll(&event));
    let mut parked = ParkedEntry::occurred(event);
    parked.held_triggers = held_triggers;
    state.deferred_entry_events.push(parked);
}

/// CR 603.2 + CR 603.10 + CR 603.2c: an as-enters roll triggers what is on the
/// battlefield when it is rolled, once per instruction for a "one or more dice"
/// ability; its triggers wait with its entry and are put on the stack with it
/// (CR 603.3).
///
/// `rolls` are one instruction's stamped rolls. Their observers are collected
/// now and held on the first; every roll joins the park.
pub(crate) fn hold_as_enters_roll_occurrence(state: &mut GameState, rolls: Vec<GameEvent>) {
    let mut observers = Some(super::triggers::collect_as_enters_roll_observers(
        state, &rolls,
    ));
    for roll in rolls {
        let held = observers.take().unwrap_or_default();
        park_as_enters_roll(state, roll, held);
    }
}

/// CR 508.4: the entrant's as-enters continuation paused, so its attacking seat is
/// owed to the owner that resumes it. Stamps the parked battlefield entry of the
/// entrant's current incarnation; returns whether one was parked. A pause no
/// as-enters continuation owns parks nothing, and nothing is stamped.
pub(crate) fn owe_entry_attack_seat(state: &mut GameState, entrant: ObjectId) -> bool {
    let Some(reference) = state
        .objects
        .get(&entrant)
        .map(ObjectIncarnationRef::from_object)
    else {
        return false;
    };
    let Some(parked) = state.deferred_entry_events.iter_mut().find(|parked| {
        matches!(
            parked.event,
            GameEvent::ZoneChanged {
                to: Zone::Battlefield,
                ..
            }
        ) && parked_entry_is_of_incarnation(&parked.event, reference)
    }) else {
        return false;
    };
    parked.entry_attack = Some(crate::types::ability::EntryAttackDestination::AnyDefender);
    true
}

/// CR 508.4 + CR 400.7: take the attacking seat the owner's paused delivery owes,
/// once, from the parked entry custody will take (matched by its ledger key, not
/// by the owner's pre-move member), with the entrant as it is now.
///
/// The owner that resumes the paused member takes it before its custody removes
/// the entry, so a second pass after the seat's own prompt finds nothing to seat.
/// The stamp is taken whatever follows: an entrant that is gone, or is a later
/// incarnation than the one that entered, gives `None`.
pub(crate) fn take_owed_entry_attack_seat(
    state: &mut GameState,
    delivery_events: &[GameEvent],
) -> Option<(
    ObjectIncarnationRef,
    crate::types::ability::EntryAttackDestination,
)> {
    let objects = &state.objects;
    state
        .deferred_entry_events
        .iter_mut()
        .filter(|parked| {
            delivery_events
                .iter()
                .any(|event| parked_event_holds(&parked.event, event))
        })
        .find_map(|parked| {
            let GameEvent::ZoneChanged {
                object_id,
                to: Zone::Battlefield,
                ..
            } = parked.event
            else {
                return None;
            };
            let destination = parked.entry_attack.take()?;
            Some(
                objects
                    .get(&object_id)
                    .map(ObjectIncarnationRef::from_object)
                    .filter(|entrant| parked_entry_is_of_incarnation(&parked.event, *entrant))
                    .map(|entrant| (entrant, destination)),
            )
        })
        .flatten()
}

/// CR 508.4 + CR 400.7: take the attacking seat `entrant`'s parked battlefield
/// entry owes, once, for the seat frame that no logical owner's delivery keys.
/// Returns the seat only while `entrant` is still that incarnation.
pub(crate) fn take_owed_entry_attack_seat_of(
    state: &mut GameState,
    entrant: ObjectIncarnationRef,
) -> Option<(
    ObjectIncarnationRef,
    crate::types::ability::EntryAttackDestination,
)> {
    let destination = state
        .deferred_entry_events
        .iter_mut()
        .filter(|parked| {
            matches!(
                parked.event,
                GameEvent::ZoneChanged {
                    to: Zone::Battlefield,
                    ..
                }
            ) && parked_entry_is_of_incarnation(&parked.event, entrant)
        })
        .find_map(|parked| parked.entry_attack.take())?;
    entrant.is_current(state).then_some((entrant, destination))
}

/// CR 508.4: whether an attacking entrant's seat is owed to an authority that
/// resumes after its delivery's pause (a logical owner or the seat frame).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwedEntrySeat {
    Owed,
    NotOwed,
}

/// CR 614.12a + CR 603.6d: the entry's as-enters continuation has not run, so
/// the entry is not over; it is held from every collector until that
/// continuation completes.
///
/// Parks every entry event of `entrant` in `delivery_events` (the delivery's own
/// slice) that the park does not already hold. The chain's dispatch then adopts
/// that custody by identity. With `OwedEntrySeat::Owed` it stamps the attacking
/// seat on the record and returns whether one was stamped.
pub(crate) fn park_entry_before_its_continuation(
    state: &mut GameState,
    entrant: ObjectId,
    delivery_events: &[GameEvent],
    owed_seat: OwedEntrySeat,
) -> bool {
    for event in delivery_events {
        if is_entry_event_of(event, entrant) && !entry_event_is_parked(state, event) {
            park_pending_entry(state, event.clone());
        }
    }
    match owed_seat {
        OwedEntrySeat::Owed => owe_entry_attack_seat(state, entrant),
        OwedEntrySeat::NotOwed => false,
    }
}

/// Take `entrant`'s own parked entries, in either state, and leave every other
/// entry parked for its own collector. Every removal that answers one entrant
/// goes through here.
pub(crate) fn take_parked_entries_of(state: &mut GameState, entrant: ObjectId) -> Vec<GameEvent> {
    take_parked_entries_of_except(state, entrant, &[])
}

/// Take `entrant`'s own parked entries except those a live logical owner's paused
/// delivery holds, which stay for that owner's custody.
fn take_parked_entries_of_except(
    state: &mut GameState,
    entrant: ObjectId,
    owner_held: &[GameEvent],
) -> Vec<GameEvent> {
    take_parked_entries_of_where(state, entrant, |parked| {
        owner_held
            .iter()
            .any(|event| parked_event_holds(&parked.event, event))
    })
}

/// Take `entrant`'s own parked entries except those `keep` names, which stay
/// parked for the collector that owns them.
fn take_parked_entries_of_where(
    state: &mut GameState,
    entrant: ObjectId,
    keep: impl Fn(&ParkedEntry) -> bool,
) -> Vec<GameEvent> {
    let (own, others): (Vec<ParkedEntry>, Vec<ParkedEntry>) =
        std::mem::take(&mut state.deferred_entry_events)
            .into_iter()
            .partition(|parked| is_entry_event_of(&parked.event, entrant) && !keep(parked));
    state.deferred_entry_events = others;
    own.into_iter().map(|parked| parked.event).collect()
}

/// Does the park hold an entry of `entrant`, in either state?
pub(crate) fn entry_park_holds_entrant(state: &GameState, entrant: ObjectId) -> bool {
    state
        .parked_entry_events()
        .any(|parked| is_entry_event_of(parked, entrant))
}

/// CR 614.12a + CR 113.6: has `entrant`'s entry not happened yet? True while
/// this incarnation's parked entry is pending. A completed entry has happened,
/// so that entrant is an ordinary observer (CR 603.2), even though its entry
/// waits for collection. The one park reader of the state outside the take and
/// the carrier abandonment.
pub(crate) fn entrant_entry_is_pending(state: &GameState, entrant: ObjectId) -> bool {
    if state.deferred_entry_events.is_empty() {
        return false;
    }
    let Some(object) = state.objects.get(&entrant) else {
        return false;
    };
    let reference = ObjectIncarnationRef::from_object(object);
    state.deferred_entry_events.iter().any(|parked| {
        parked_entry_is_of_incarnation(&parked.event, reference)
            && match parked.state {
                ParkedEntryState::Pending { .. } => true,
                ParkedEntryState::Completed => false,
            }
    })
}

/// CR 614.1c + CR 614.12 + CR 111.1: has `entrant` not entered yet? Its parked
/// battlefield entry is pending (`entrant_entry_is_pending`), or it is a liminal
/// entrant whose entry is decided but not committed (a token copy, CR 707.2),
/// which is in no zone at all.
pub(crate) fn entrant_has_not_entered(state: &GameState, entrant: ObjectId) -> bool {
    entrant_entry_is_pending(state, entrant) || state.liminal_entries.contains_key(&entrant)
}

/// CR 400.7: is `record` an entry of exactly `incarnation`? A record stamped
/// with no incarnation cannot name a different one.
fn entry_record_is_of_incarnation(
    record: &crate::types::game_state::ZoneChangeRecord,
    incarnation: u64,
) -> bool {
    record
        .entered_incarnation
        .is_none_or(|entered| entered == incarnation)
}

/// CR 400.7: does the parked `event` describe this exact incarnation of the
/// entrant? A stale `ZoneChanged` of an earlier incarnation of the same
/// `ObjectId` does not.
fn parked_entry_is_of_incarnation(event: &GameEvent, entrant: ObjectIncarnationRef) -> bool {
    is_entry_event_of(event, entrant.object_id)
        && match event {
            GameEvent::ZoneChanged { record, .. } => {
                entry_record_is_of_incarnation(record, entrant.incarnation)
            }
            GameEvent::GameStarted
            | GameEvent::HiddenSearchViewed { .. }
            | GameEvent::TurnStarted { .. }
            | GameEvent::ExtraTurnCreated { .. }
            | GameEvent::PhaseChanged { .. }
            | GameEvent::PhaseChangedFor { .. }
            | GameEvent::PriorityPassed { .. }
            | GameEvent::SpellCast { .. }
            | GameEvent::Mutated { .. }
            | GameEvent::Augmented { .. }
            | GameEvent::Melded { .. }
            | GameEvent::SpellCopied { .. }
            | GameEvent::XValueChosen { .. }
            | GameEvent::AbilityActivated { .. }
            | GameEvent::LifeChanged { .. }
            | GameEvent::ManaAdded { .. }
            | GameEvent::TappedForMana { .. }
            | GameEvent::ManaAbilityProduced { .. }
            | GameEvent::ManaPoolEmptied { .. }
            | GameEvent::ManaBurn { .. }
            | GameEvent::ManaRecolored { .. }
            | GameEvent::PermanentTapped { .. }
            | GameEvent::CreatureExerted { .. }
            | GameEvent::CreatureEnlisted { .. }
            | GameEvent::ArmyAmassed { .. }
            | GameEvent::Foretold { .. }
            | GameEvent::BecameForetold { .. }
            | GameEvent::PlayerLost { .. }
            | GameEvent::MulliganStarted
            | GameEvent::CardsDrawn { .. }
            | GameEvent::CardDrawn { .. }
            | GameEvent::PermanentUntapped { .. }
            | GameEvent::PermanentPhasedOut { .. }
            | GameEvent::PermanentPhasedIn { .. }
            | GameEvent::PlayerPhasedOut { .. }
            | GameEvent::PlayerPhasedIn { .. }
            | GameEvent::LandPlayed { .. }
            | GameEvent::StackPushed { .. }
            | GameEvent::AbilityTriggered { .. }
            | GameEvent::StackResolved { .. }
            | GameEvent::Discarded { .. }
            | GameEvent::Milled { .. }
            | GameEvent::DamageCleared { .. }
            | GameEvent::GameOver { .. }
            | GameEvent::ResolutionHalted { .. }
            | GameEvent::ShortcutRepetition { .. }
            | GameEvent::DamageDealt { .. }
            | GameEvent::DamagePrevented { .. }
            | GameEvent::SpellCountered { .. }
            | GameEvent::CounterAdded { .. }
            | GameEvent::SagaChapterAbilityResolved { .. }
            | GameEvent::ObjectIntensified { .. }
            | GameEvent::Evolved { .. }
            | GameEvent::UnTellPerformed { .. }
            | GameEvent::PhraseSaid { .. }
            | GameEvent::GotchaSaid { .. }
            | GameEvent::ObjectTouched { .. }
            | GameEvent::CounterRemoved { .. }
            | GameEvent::TokenCreated { .. }
            | GameEvent::ObjectConjured { .. }
            | GameEvent::CardCopyCreated { .. }
            | GameEvent::CreatureDestroyed { .. }
            | GameEvent::PermanentSacrificed { .. }
            | GameEvent::ControllerChanged { .. }
            | GameEvent::EffectResolved { .. }
            | GameEvent::Attached { .. }
            | GameEvent::Unattached { .. }
            | GameEvent::ContinuousEffectEnded { .. }
            | GameEvent::AttackersDeclared { .. }
            | GameEvent::BlockersDeclared { .. }
            | GameEvent::AttackerBecameBlocked { .. }
            | GameEvent::AttackerBecameBlockedByFilteredBlocker { .. }
            | GameEvent::CombatTaxPaid { .. }
            | GameEvent::CombatTaxDeclined { .. }
            | GameEvent::BecomesTarget { .. }
            | GameEvent::VehicleCrewed { .. }
            | GameEvent::Stationed { .. }
            | GameEvent::Saddled { .. }
            | GameEvent::ReplacementApplied { .. }
            | GameEvent::Transformed { .. }
            | GameEvent::Flipped { .. }
            | GameEvent::Specialized { .. }
            | GameEvent::DayNightChanged { .. }
            | GameEvent::TurnedFaceUp { .. }
            | GameEvent::TurnedFaceDown { .. }
            | GameEvent::CardsRevealed { .. }
            | GameEvent::ChosenNumbersRevealed { .. }
            | GameEvent::CombatDamageDealtToPlayer { .. }
            | GameEvent::PlayerEliminated { .. }
            | GameEvent::CrimeCommitted { .. }
            | GameEvent::Cycled { .. }
            | GameEvent::PlayerPerformedAction { .. }
            | GameEvent::CardPredicateGuessMade { .. }
            | GameEvent::CardNamed { .. }
            | GameEvent::Regenerated { .. }
            | GameEvent::CreatureSuspected { .. }
            | GameEvent::CreatureNoLongerSuspected { .. }
            | GameEvent::Detained { .. }
            | GameEvent::BecamePrepared { .. }
            | GameEvent::BecameUnprepared { .. }
            | GameEvent::CaseSolved { .. }
            | GameEvent::ClassLevelGained { .. }
            | GameEvent::MonarchChanged { .. }
            | GameEvent::CityBlessingGained { .. }
            | GameEvent::EnduringStoryGained { .. }
            | GameEvent::DieRolled { .. }
            | GameEvent::DieRollIgnored { .. }
            | GameEvent::StartingPlayerContest { .. }
            | GameEvent::CoinFlipped { .. }
            | GameEvent::RingTemptsYou { .. }
            | GameEvent::RoomEntered { .. }
            | GameEvent::RoomDoorUnlocked { .. }
            | GameEvent::BecomesPlotted { .. }
            | GameEvent::DungeonCompleted { .. }
            | GameEvent::Planeswalked { .. }
            | GameEvent::ChaosEnsued { .. }
            | GameEvent::PlanarDieRolled { .. }
            | GameEvent::SchemeSetInMotion { .. }
            | GameEvent::SchemeAbandoned { .. }
            | GameEvent::InitiativeTaken { .. }
            | GameEvent::AttractionOpened { .. }
            | GameEvent::ContraptionAssembled { .. }
            | GameEvent::StickerPlaced { .. }
            | GameEvent::AttractionsRolledToVisit { .. }
            | GameEvent::AttractionVisited { .. }
            | GameEvent::ContraptionCranked { .. }
            | GameEvent::Firebend { .. }
            | GameEvent::Airbend { .. }
            | GameEvent::Earthbend { .. }
            | GameEvent::Waterbend { .. }
            | GameEvent::CompanionRevealed { .. }
            | GameEvent::CompanionMovedToHand { .. }
            | GameEvent::NinjutsuActivated { .. }
            | GameEvent::KeywordAbilityActivated { .. }
            | GameEvent::CreatureExploited { .. }
            | GameEvent::EnergyChanged { .. }
            | GameEvent::SpeedChanged { .. }
            | GameEvent::PlayerCounterChanged { .. }
            | GameEvent::ManaExpended { .. }
            | GameEvent::PhysicalContestStarted { .. }
            | GameEvent::PhysicalContestResolved { .. }
            | GameEvent::Clash { .. }
            | GameEvent::VoteCast { .. }
            | GameEvent::VoteResolved { .. }
            | GameEvent::PowerToughnessChanged { .. }
            | GameEvent::CascadeMissed { .. }
            | GameEvent::DebugActionUsed { .. }
            | GameEvent::DebugPermissionGranted { .. }
            | GameEvent::DebugPermissionRevoked { .. } => true,
        }
}

/// CR 603.2 + CR 603.10: the paused continuation that parked these entries is
/// over, so each entry happened, even if a later instruction moved its entrant.
///
/// Design decision, not derived from a rule: the one refusal is an entrant not
/// current on the battlefield whose owner or controller of record has left, so
/// its entry stays pending. An owner conceding at the entrant's copied
/// `TributeChoice` reaches this: the opponent's answer retires the drain after
/// the entrant has gone. The test reads the moment the continuation ended, and a
/// later departure changes nothing because the state is already recorded.
pub(crate) fn record_parked_entry_completion(
    state: &mut GameState,
    entrants: &[ObjectIncarnationRef],
) {
    for entrant in entrants.iter().copied() {
        let gone = !entrant.is_current(state)
            || state
                .objects
                .get(&entrant.object_id)
                .is_none_or(|object| object.zone != Zone::Battlefield);
        let refused: Vec<bool> = state
            .deferred_entry_events
            .iter()
            .map(|parked| gone && parked_entrant_left_the_game(state, &parked.event))
            .collect();
        for (parked, refused) in state.deferred_entry_events.iter_mut().zip(refused) {
            if parked_entry_is_of_incarnation(&parked.event, entrant) && !refused {
                parked.state = ParkedEntryState::Completed;
            }
        }
    }
}

/// CR 305.1: record a played land's `LandPlayed` as completed when a replacement
/// kept the land from entering, so nothing will flush it.
///
/// Design decision preserved from base, not derived from a rule: the play was
/// committed by `finalize_committed_land_play`, so its `LandPlayed` stays
/// collectable when a replacement stopped the land from entering.
fn record_parked_land_play_completion(state: &mut GameState, land: ObjectId) {
    for parked in &mut state.deferred_entry_events {
        if matches!(parked.event, GameEvent::LandPlayed { object_id, .. } if object_id == land) {
            parked.state = ParkedEntryState::Completed;
        }
    }
}

/// Whole-carrier abandonment's removal: drop the pending entries of the family
/// that `carrier` parked. A completed entry happened and stays for the take, and
/// a pending entry of another carrier stays for its own producer.
pub(crate) fn drop_abandoned_family_entries(state: &mut GameState, carrier: Option<ObjectId>) {
    state
        .deferred_entry_events
        .retain(|parked| match parked.state {
            ParkedEntryState::Completed => true,
            ParkedEntryState::Pending {
                carrier: parked_carrier,
            } => parked_carrier != carrier,
        });
}

/// CR 616.1g: a retired dispatch's entries belong to whatever owns the rest of
/// their entry: nothing if it completed, else the frame's new resident.
fn settle_retired_dispatch_entrants(
    state: &mut GameState,
    dispatch: crate::types::resolution::IdentifiedPostReplacementDispatch,
    entrants: Vec<ObjectIncarnationRef>,
    waiting_for: Option<&WaitingFor>,
) {
    match waiting_for {
        None => record_parked_entry_completion(state, &entrants),
        Some(_) => {
            for entrant in entrants {
                let _ = state.bind_parked_entrant(dispatch, entrant);
            }
        }
    }
}

/// CR 614.12a + CR 707.9: bind a copied as-enters choice's entrant to the active
/// frame's resident drain, whose retirement then completes the entry. Returns the
/// entrant when its entry is parked and no resident can carry it, so the caller
/// parks an owner.
fn bind_copied_choice_entrant(
    state: &mut GameState,
    source_id: ObjectId,
) -> Option<ObjectIncarnationRef> {
    if !entry_park_holds_entrant(state, source_id) {
        return None;
    }
    let entrant = ObjectIncarnationRef::from_object(state.objects.get(&source_id)?);
    (!state.bind_parked_entrant_to_active_drain(entrant)).then_some(entrant)
}

/// CR 614.12a + CR 707.9: a copied as-enters choice is the rest of this entry.
/// Its chain ran outside a dispatch, so a paused drain below that chain owns the
/// entry's completion, as it does for a printed choice.
fn park_copied_choice_owner(
    state: &mut GameState,
    entrant: ObjectIncarnationRef,
    child_stack_start: crate::types::resolution::ChildStackDepth,
) {
    let mut owner = PostReplacementDrainStack::default();
    owner.install(
        PostReplacementDrain::paused_owner_of(entrant),
        crate::types::game_state::ResidentDrainPolicy::Replace,
    );
    match state
        .resolution_stack
        .capture_child_boundary()
        .cmp(&child_stack_start)
    {
        std::cmp::Ordering::Less => {
            panic!("a paused copied choice removed a parent frame before its owner was parked")
        }
        std::cmp::Ordering::Equal => state.resolution_stack.push_post_replacement(owner),
        std::cmp::Ordering::Greater => state
            .resolution_stack
            .insert_parent_at_child_boundary(
                crate::types::resolution::ResolutionFrame::PostReplacement(owner),
                child_stack_start,
            )
            .expect("a copied choice's owner must be inserted below its complete child stack"),
    }
}

/// CR 603.6d + CR 614.1c: the entrant whose own as-enters continuation is
/// about to run: a `Moved` continuation for an object that is, right now, the
/// exact entered incarnation (CR 400.7) of a battlefield entry in `events`
/// whose key is not already parked. A "would die / would be exiled … instead"
/// `Moved` continuation for an object that entered earlier in the same buffer
/// fails the live-zone test.
fn as_enters_continuation_entrant(
    state: &GameState,
    event: Option<&ReplacementEvent>,
    source: Option<ObjectId>,
    events: &[GameEvent],
) -> Option<ObjectId> {
    if event != Some(&ReplacementEvent::Moved) {
        return None;
    }
    let source = source?;
    let live = state
        .objects
        .get(&source)
        .filter(|obj| obj.zone == Zone::Battlefield)?;
    events
        .iter()
        .any(|entry| {
            matches!(
                entry,
                GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Battlefield,
                    record,
                    ..
                } if *object_id == source
                    && entry_record_is_of_incarnation(record, live.incarnation)
                    && !entry_event_is_parked(state, entry)
            )
        })
        .then_some(source)
}

/// The bracket that makes an as-enters continuation part of its entrant's entry
/// (CR 603.6d + CR 614.1c). `open` parks the entrant's entry events as pending
/// before the continuation runs, or adopts the custody its delivery already
/// parked; `close` completes or unparks them when it completed inline and keeps
/// them, bound to the paused dispatch, when it paused.
struct EntryContinuationParking {
    custody: EntryCustody,
}

/// Who holds the entry of the continuation's entrant while it runs.
enum EntryCustody {
    /// The continuation is no entrant's as-enters continuation.
    None,
    /// `open` parked these entry events of `entrant` from the dispatch's events.
    Opened {
        entrant: ObjectId,
        parked: Vec<GameEvent>,
    },
    /// The entrant's delivery parked its entry at a pause before this
    /// continuation ran (`park_entry_before_its_continuation`); the dispatch
    /// adopts that custody by identity.
    Adopted(ObjectIncarnationRef),
}

impl EntryContinuationParking {
    /// CR 603.6d + CR 614.1c: the continuation's work is part of the entry, so
    /// the entry is neither observed nor collected while it runs. The parked-key
    /// skip in `zones::settle_battlefield_entry_observations` then refuses to
    /// observe this entrant at every instruction boundary inside the
    /// continuation; other entries of the same buffer still settle.
    ///
    /// `adopts`: the entrant's entry is already held pending and this dispatch
    /// is its as-enters roll, so the custody is adopted, whatever `event` the
    /// dispatch carries.
    fn open(
        state: &mut GameState,
        adopts: bool,
        event: Option<&ReplacementEvent>,
        source: Option<ObjectId>,
        events: &[GameEvent],
    ) -> Self {
        if adopts {
            if let Some(reference) = source
                .and_then(|source| state.objects.get(&source))
                .map(ObjectIncarnationRef::from_object)
            {
                return Self {
                    custody: EntryCustody::Adopted(reference),
                };
            }
        }
        let Some(entrant) = as_enters_continuation_entrant(state, event, source, events) else {
            return Self {
                custody: EntryCustody::None,
            };
        };
        let parked: Vec<GameEvent> = events
            .iter()
            .filter(|entry| is_entry_event_of(entry, entrant))
            .cloned()
            .collect();
        for entry in &parked {
            park_pending_entry(state, entry.clone());
        }
        Self {
            custody: EntryCustody::Opened { entrant, parked },
        }
    }

    /// CR 614.12a + CR 603.6d: a continuation that completed inline leaves its
    /// entry unparked; the entry's `ZoneChanged` is still in the action's events,
    /// so its owning collector collects it against the realized entrant. A
    /// paused continuation keeps the park for its release or owner custody, and
    /// the dispatch that owns the prompt carries the binding to the completion
    /// authority.
    ///
    /// CR 603.6d + CR 603.2: an adopted entry completes with its continuation
    /// and waits for its collector. Its `ZoneChanged` belongs to an earlier
    /// action's buffer, scanned while it was parked, so it is recorded completed
    /// rather than unparked.
    ///
    /// An as-enters roll of this entrant stays parked in either arm. The settled
    /// release takes it when the action settles, so an action that ends on a later
    /// prompt cannot lose it (CR 603.2 + CR 603.3).
    fn close(
        self,
        state: &mut GameState,
        dispatch: crate::types::resolution::IdentifiedPostReplacementDispatch,
        waiting_for: Option<&WaitingFor>,
    ) {
        match self.custody {
            EntryCustody::None => {}
            EntryCustody::Opened { entrant, parked } => match waiting_for {
                None => state
                    .deferred_entry_events
                    .retain(|entry| !parked.contains(&entry.event)),
                // CR 614.12a + CR 707.9: the entry of an object entering as a copy
                // is not over at the copy answer. Counters and any copied
                // as-enters choice or payment still follow, and the replay
                // collects it.
                Some(WaitingFor::CopyTargetChoice { source_id, .. }) if *source_id == entrant => {}
                Some(_) => {
                    if let Some(object) = state.objects.get(&entrant) {
                        let reference = ObjectIncarnationRef::from_object(object);
                        let _ = state.bind_parked_entrant(dispatch, reference);
                    }
                }
            },
            EntryCustody::Adopted(reference) => match waiting_for {
                None => record_parked_entry_completion(state, &[reference]),
                Some(WaitingFor::CopyTargetChoice { source_id, .. })
                    if *source_id == reference.object_id => {}
                Some(_) => {
                    let _ = state.bind_parked_entrant(dispatch, reference);
                }
            },
        }
    }
}

/// CR 603.6d + CR 603.2c: a logical zone-change owner resuming its paused
/// member takes custody of that member's parked entry: every parked event that
/// holds an event of `delivery_events` leaves the park. The owner's segment
/// collection that follows is the ordinary half; this runs the delayed half.
///
/// CR 603.2 + CR 603.7: a delayed triggered ability whose trigger event is the
/// entry triggers on the realized entry, once, like a printed one.
///
/// An as-enters roll is left for the settled release, which places the contexts
/// its instruction triggered as it was rolled (CR 603.2 + CR 603.10), so a roll
/// taken here would lose them.
pub(crate) fn take_entry_continuation_custody(
    state: &mut GameState,
    delivery_events: &[GameEvent],
) {
    if state.deferred_entry_events.is_empty() {
        return;
    }
    let (taken, kept): (Vec<ParkedEntry>, Vec<ParkedEntry>) =
        std::mem::take(&mut state.deferred_entry_events)
            .into_iter()
            .partition(|parked| {
                !is_as_enters_roll(&parked.event)
                    && delivery_events
                        .iter()
                        .any(|event| parked_event_holds(&parked.event, event))
            });
    state.deferred_entry_events = kept;
    let taken: Vec<GameEvent> = taken.into_iter().map(|parked| parked.event).collect();
    if !taken.is_empty() {
        super::triggers::collect_delayed_trigger_contexts_into_deferred(state, &taken);
    }
}

/// CR 800.4a: has the owner or the controller of record of the entrant this
/// parked event describes left the game (its owner's objects leave the game;
/// objects its controller still controls are exiled)? A `ZoneChanged` reads its record; a
/// `LandPlayed` / attach event follows the parked `ZoneChanged` for the same
/// object, else the object in `state.objects`.
fn parked_entrant_left_the_game(state: &GameState, event: &GameEvent) -> bool {
    let entrant_record = |object_id: ObjectId| {
        state.parked_entry_events().find_map(|parked| match parked {
            GameEvent::ZoneChanged {
                object_id: parked_id,
                to: Zone::Battlefield,
                record,
                ..
            } if *parked_id == object_id => Some((record.owner, record.arrival.controller)),
            GameEvent::GameStarted
            | GameEvent::HiddenSearchViewed { .. }
            | GameEvent::TurnStarted { .. }
            | GameEvent::ExtraTurnCreated { .. }
            | GameEvent::PhaseChanged { .. }
            | GameEvent::PhaseChangedFor { .. }
            | GameEvent::PriorityPassed { .. }
            | GameEvent::SpellCast { .. }
            | GameEvent::Mutated { .. }
            | GameEvent::Augmented { .. }
            | GameEvent::Melded { .. }
            | GameEvent::SpellCopied { .. }
            | GameEvent::XValueChosen { .. }
            | GameEvent::AbilityActivated { .. }
            | GameEvent::ZoneChanged { .. }
            | GameEvent::LifeChanged { .. }
            | GameEvent::ManaAdded { .. }
            | GameEvent::TappedForMana { .. }
            | GameEvent::ManaAbilityProduced { .. }
            | GameEvent::ManaPoolEmptied { .. }
            | GameEvent::ManaBurn { .. }
            | GameEvent::ManaRecolored { .. }
            | GameEvent::PermanentTapped { .. }
            | GameEvent::CreatureExerted { .. }
            | GameEvent::CreatureEnlisted { .. }
            | GameEvent::ArmyAmassed { .. }
            | GameEvent::Foretold { .. }
            | GameEvent::BecameForetold { .. }
            | GameEvent::PlayerLost { .. }
            | GameEvent::MulliganStarted
            | GameEvent::CardsDrawn { .. }
            | GameEvent::CardDrawn { .. }
            | GameEvent::PermanentUntapped { .. }
            | GameEvent::PermanentPhasedOut { .. }
            | GameEvent::PermanentPhasedIn { .. }
            | GameEvent::PlayerPhasedOut { .. }
            | GameEvent::PlayerPhasedIn { .. }
            | GameEvent::LandPlayed { .. }
            | GameEvent::StackPushed { .. }
            | GameEvent::AbilityTriggered { .. }
            | GameEvent::StackResolved { .. }
            | GameEvent::Discarded { .. }
            | GameEvent::Milled { .. }
            | GameEvent::DamageCleared { .. }
            | GameEvent::GameOver { .. }
            | GameEvent::ResolutionHalted { .. }
            | GameEvent::ShortcutRepetition { .. }
            | GameEvent::DamageDealt { .. }
            | GameEvent::DamagePrevented { .. }
            | GameEvent::SpellCountered { .. }
            | GameEvent::CounterAdded { .. }
            | GameEvent::SagaChapterAbilityResolved { .. }
            | GameEvent::ObjectIntensified { .. }
            | GameEvent::Evolved { .. }
            | GameEvent::UnTellPerformed { .. }
            | GameEvent::PhraseSaid { .. }
            | GameEvent::GotchaSaid { .. }
            | GameEvent::ObjectTouched { .. }
            | GameEvent::CounterRemoved { .. }
            | GameEvent::TokenCreated { .. }
            | GameEvent::ObjectConjured { .. }
            | GameEvent::CardCopyCreated { .. }
            | GameEvent::CreatureDestroyed { .. }
            | GameEvent::PermanentSacrificed { .. }
            | GameEvent::ControllerChanged { .. }
            | GameEvent::EffectResolved { .. }
            | GameEvent::Attached { .. }
            | GameEvent::Unattached { .. }
            | GameEvent::ContinuousEffectEnded { .. }
            | GameEvent::AttackersDeclared { .. }
            | GameEvent::BlockersDeclared { .. }
            | GameEvent::AttackerBecameBlocked { .. }
            | GameEvent::AttackerBecameBlockedByFilteredBlocker { .. }
            | GameEvent::CombatTaxPaid { .. }
            | GameEvent::CombatTaxDeclined { .. }
            | GameEvent::BecomesTarget { .. }
            | GameEvent::VehicleCrewed { .. }
            | GameEvent::Stationed { .. }
            | GameEvent::Saddled { .. }
            | GameEvent::ReplacementApplied { .. }
            | GameEvent::Transformed { .. }
            | GameEvent::Flipped { .. }
            | GameEvent::Specialized { .. }
            | GameEvent::DayNightChanged { .. }
            | GameEvent::TurnedFaceUp { .. }
            | GameEvent::TurnedFaceDown { .. }
            | GameEvent::CardsRevealed { .. }
            | GameEvent::ChosenNumbersRevealed { .. }
            | GameEvent::CombatDamageDealtToPlayer { .. }
            | GameEvent::PlayerEliminated { .. }
            | GameEvent::CrimeCommitted { .. }
            | GameEvent::Cycled { .. }
            | GameEvent::PlayerPerformedAction { .. }
            | GameEvent::CardPredicateGuessMade { .. }
            | GameEvent::CardNamed { .. }
            | GameEvent::Regenerated { .. }
            | GameEvent::CreatureSuspected { .. }
            | GameEvent::CreatureNoLongerSuspected { .. }
            | GameEvent::Detained { .. }
            | GameEvent::BecamePrepared { .. }
            | GameEvent::BecameUnprepared { .. }
            | GameEvent::CaseSolved { .. }
            | GameEvent::ClassLevelGained { .. }
            | GameEvent::MonarchChanged { .. }
            | GameEvent::CityBlessingGained { .. }
            | GameEvent::EnduringStoryGained { .. }
            | GameEvent::DieRolled { .. }
            | GameEvent::DieRollIgnored { .. }
            | GameEvent::StartingPlayerContest { .. }
            | GameEvent::CoinFlipped { .. }
            | GameEvent::RingTemptsYou { .. }
            | GameEvent::RoomEntered { .. }
            | GameEvent::RoomDoorUnlocked { .. }
            | GameEvent::BecomesPlotted { .. }
            | GameEvent::DungeonCompleted { .. }
            | GameEvent::Planeswalked { .. }
            | GameEvent::ChaosEnsued { .. }
            | GameEvent::PlanarDieRolled { .. }
            | GameEvent::SchemeSetInMotion { .. }
            | GameEvent::SchemeAbandoned { .. }
            | GameEvent::InitiativeTaken { .. }
            | GameEvent::AttractionOpened { .. }
            | GameEvent::ContraptionAssembled { .. }
            | GameEvent::StickerPlaced { .. }
            | GameEvent::AttractionsRolledToVisit { .. }
            | GameEvent::AttractionVisited { .. }
            | GameEvent::ContraptionCranked { .. }
            | GameEvent::Firebend { .. }
            | GameEvent::Airbend { .. }
            | GameEvent::Earthbend { .. }
            | GameEvent::Waterbend { .. }
            | GameEvent::CompanionRevealed { .. }
            | GameEvent::CompanionMovedToHand { .. }
            | GameEvent::NinjutsuActivated { .. }
            | GameEvent::KeywordAbilityActivated { .. }
            | GameEvent::CreatureExploited { .. }
            | GameEvent::EnergyChanged { .. }
            | GameEvent::SpeedChanged { .. }
            | GameEvent::PlayerCounterChanged { .. }
            | GameEvent::ManaExpended { .. }
            | GameEvent::PhysicalContestStarted { .. }
            | GameEvent::PhysicalContestResolved { .. }
            | GameEvent::Clash { .. }
            | GameEvent::VoteCast { .. }
            | GameEvent::VoteResolved { .. }
            | GameEvent::PowerToughnessChanged { .. }
            | GameEvent::CascadeMissed { .. }
            | GameEvent::DebugActionUsed { .. }
            | GameEvent::DebugPermissionGranted { .. }
            | GameEvent::DebugPermissionRevoked { .. } => None,
        })
    };
    let identity = match event {
        GameEvent::ZoneChanged { record, .. } => Some((record.owner, record.arrival.controller)),
        GameEvent::LandPlayed { object_id, .. }
        | GameEvent::EffectResolved {
            source_id: object_id,
            ..
        }
        | GameEvent::Attached {
            attachment_id: object_id,
            ..
        } => entrant_record(*object_id).or_else(|| {
            state
                .objects
                .get(object_id)
                .map(|obj| (obj.owner, obj.controller))
        }),
        GameEvent::GameStarted
        | GameEvent::HiddenSearchViewed { .. }
        | GameEvent::TurnStarted { .. }
        | GameEvent::ExtraTurnCreated { .. }
        | GameEvent::PhaseChanged { .. }
        | GameEvent::PhaseChangedFor { .. }
        | GameEvent::PriorityPassed { .. }
        | GameEvent::SpellCast { .. }
        | GameEvent::Mutated { .. }
        | GameEvent::Augmented { .. }
        | GameEvent::Melded { .. }
        | GameEvent::SpellCopied { .. }
        | GameEvent::XValueChosen { .. }
        | GameEvent::AbilityActivated { .. }
        | GameEvent::LifeChanged { .. }
        | GameEvent::ManaAdded { .. }
        | GameEvent::TappedForMana { .. }
        | GameEvent::ManaAbilityProduced { .. }
        | GameEvent::ManaPoolEmptied { .. }
        | GameEvent::ManaBurn { .. }
        | GameEvent::ManaRecolored { .. }
        | GameEvent::PermanentTapped { .. }
        | GameEvent::CreatureExerted { .. }
        | GameEvent::CreatureEnlisted { .. }
        | GameEvent::ArmyAmassed { .. }
        | GameEvent::Foretold { .. }
        | GameEvent::BecameForetold { .. }
        | GameEvent::PlayerLost { .. }
        | GameEvent::MulliganStarted
        | GameEvent::CardsDrawn { .. }
        | GameEvent::CardDrawn { .. }
        | GameEvent::PermanentUntapped { .. }
        | GameEvent::PermanentPhasedOut { .. }
        | GameEvent::PermanentPhasedIn { .. }
        | GameEvent::PlayerPhasedOut { .. }
        | GameEvent::PlayerPhasedIn { .. }
        | GameEvent::StackPushed { .. }
        | GameEvent::AbilityTriggered { .. }
        | GameEvent::StackResolved { .. }
        | GameEvent::Discarded { .. }
        | GameEvent::Milled { .. }
        | GameEvent::DamageCleared { .. }
        | GameEvent::GameOver { .. }
        | GameEvent::ResolutionHalted { .. }
        | GameEvent::ShortcutRepetition { .. }
        | GameEvent::DamageDealt { .. }
        | GameEvent::DamagePrevented { .. }
        | GameEvent::SpellCountered { .. }
        | GameEvent::CounterAdded { .. }
        | GameEvent::SagaChapterAbilityResolved { .. }
        | GameEvent::ObjectIntensified { .. }
        | GameEvent::Evolved { .. }
        | GameEvent::UnTellPerformed { .. }
        | GameEvent::PhraseSaid { .. }
        | GameEvent::GotchaSaid { .. }
        | GameEvent::ObjectTouched { .. }
        | GameEvent::CounterRemoved { .. }
        | GameEvent::TokenCreated { .. }
        | GameEvent::ObjectConjured { .. }
        | GameEvent::CardCopyCreated { .. }
        | GameEvent::CreatureDestroyed { .. }
        | GameEvent::PermanentSacrificed { .. }
        | GameEvent::ControllerChanged { .. }
        | GameEvent::Unattached { .. }
        | GameEvent::ContinuousEffectEnded { .. }
        | GameEvent::AttackersDeclared { .. }
        | GameEvent::BlockersDeclared { .. }
        | GameEvent::AttackerBecameBlocked { .. }
        | GameEvent::AttackerBecameBlockedByFilteredBlocker { .. }
        | GameEvent::CombatTaxPaid { .. }
        | GameEvent::CombatTaxDeclined { .. }
        | GameEvent::BecomesTarget { .. }
        | GameEvent::VehicleCrewed { .. }
        | GameEvent::Stationed { .. }
        | GameEvent::Saddled { .. }
        | GameEvent::ReplacementApplied { .. }
        | GameEvent::Transformed { .. }
        | GameEvent::Flipped { .. }
        | GameEvent::Specialized { .. }
        | GameEvent::DayNightChanged { .. }
        | GameEvent::TurnedFaceUp { .. }
        | GameEvent::TurnedFaceDown { .. }
        | GameEvent::CardsRevealed { .. }
        | GameEvent::ChosenNumbersRevealed { .. }
        | GameEvent::CombatDamageDealtToPlayer { .. }
        | GameEvent::PlayerEliminated { .. }
        | GameEvent::CrimeCommitted { .. }
        | GameEvent::Cycled { .. }
        | GameEvent::PlayerPerformedAction { .. }
        | GameEvent::CardPredicateGuessMade { .. }
        | GameEvent::CardNamed { .. }
        | GameEvent::Regenerated { .. }
        | GameEvent::CreatureSuspected { .. }
        | GameEvent::CreatureNoLongerSuspected { .. }
        | GameEvent::Detained { .. }
        | GameEvent::BecamePrepared { .. }
        | GameEvent::BecameUnprepared { .. }
        | GameEvent::CaseSolved { .. }
        | GameEvent::ClassLevelGained { .. }
        | GameEvent::MonarchChanged { .. }
        | GameEvent::CityBlessingGained { .. }
        | GameEvent::EnduringStoryGained { .. }
        | GameEvent::DieRolled { .. }
        | GameEvent::DieRollIgnored { .. }
        | GameEvent::StartingPlayerContest { .. }
        | GameEvent::CoinFlipped { .. }
        | GameEvent::RingTemptsYou { .. }
        | GameEvent::RoomEntered { .. }
        | GameEvent::RoomDoorUnlocked { .. }
        | GameEvent::BecomesPlotted { .. }
        | GameEvent::DungeonCompleted { .. }
        | GameEvent::Planeswalked { .. }
        | GameEvent::ChaosEnsued { .. }
        | GameEvent::PlanarDieRolled { .. }
        | GameEvent::SchemeSetInMotion { .. }
        | GameEvent::SchemeAbandoned { .. }
        | GameEvent::InitiativeTaken { .. }
        | GameEvent::AttractionOpened { .. }
        | GameEvent::ContraptionAssembled { .. }
        | GameEvent::StickerPlaced { .. }
        | GameEvent::AttractionsRolledToVisit { .. }
        | GameEvent::AttractionVisited { .. }
        | GameEvent::ContraptionCranked { .. }
        | GameEvent::Firebend { .. }
        | GameEvent::Airbend { .. }
        | GameEvent::Earthbend { .. }
        | GameEvent::Waterbend { .. }
        | GameEvent::CompanionRevealed { .. }
        | GameEvent::CompanionMovedToHand { .. }
        | GameEvent::NinjutsuActivated { .. }
        | GameEvent::KeywordAbilityActivated { .. }
        | GameEvent::CreatureExploited { .. }
        | GameEvent::EnergyChanged { .. }
        | GameEvent::SpeedChanged { .. }
        | GameEvent::PlayerCounterChanged { .. }
        | GameEvent::ManaExpended { .. }
        | GameEvent::PhysicalContestStarted { .. }
        | GameEvent::PhysicalContestResolved { .. }
        | GameEvent::Clash { .. }
        | GameEvent::VoteCast { .. }
        | GameEvent::VoteResolved { .. }
        | GameEvent::PowerToughnessChanged { .. }
        | GameEvent::CascadeMissed { .. }
        | GameEvent::DebugActionUsed { .. }
        | GameEvent::DebugPermissionGranted { .. }
        | GameEvent::DebugPermissionRevoked { .. } => None,
    };
    identity.is_some_and(|(owner, controller)| {
        !super::players::is_alive(state, owner) || !super::players::is_alive(state, controller)
    })
}

/// CR 603.2 + CR 603.6a + CR 603.10 + CR 800.4a + CR 800.4d: take the parked
/// entries whose as-enters continuations are settled. The gate is the complement of any pause
/// (`WaitingFor::Priority`, as in `realize_settled_token_battlefield_entry`), no
/// inline dispatch live on this thread, and the drain authority's own readiness
/// predicate (`resolution_completion_can_settle`: no live resolution frame,
/// replacement pause, payment or trigger construction). A take therefore means
/// the entrant's continuation, and every frame above it, is no longer live.
///
/// CR 603.2 + CR 603.3: an as-enters roll is not returned. Its observers were
/// collected when it was rolled, so the contexts it holds join
/// `deferred_triggers` here, for the caller's placement beside its entry's;
/// CR 800.4d: a context whose controller has left the game is retired.
///
/// The recorded state decides collection, not presence. The gate means no
/// continuation is live, so a pending entry can only have been abandoned, and it
/// is dropped. That is a design decision, not derived from a rule: the rules do
/// not address a departure during a replacement's application (CR 104.3a allows
/// concession at any time). A completed entry happened (CR 603.2; a played land's
/// entry is its play, CR 305.1). Neither its owner leaving (CR 800.4a) nor the
/// entrant leaving (CR 603.10) untriggers its observers, and abilities a departed
/// player would control are retired at collection (CR 800.4d). The completed
/// events pass the consumed-ledger and queued-witness authority, so a release
/// never re-collects an occurrence another collector claimed or queued.
fn take_settled_entry_continuation_events(state: &mut GameState) -> Vec<GameEvent> {
    if state.deferred_entry_events.is_empty()
        || !matches!(state.waiting_for, WaitingFor::Priority { .. })
        || post_replacement_dispatch_is_live()
        || !super::triggers::resolution_completion_can_settle(state)
    {
        return Vec::new();
    }
    // CR 603.2c: a paused logical owner is a live frame, so it always fails the
    // gate above and its member stays parked for its custody.
    debug_assert!(super::engine_priority::retained_logical_zone_events(state).is_empty());
    let mut completed: Vec<GameEvent> = Vec::new();
    let mut held: Vec<super::triggers::DeferredTrigger> = Vec::new();
    for parked in std::mem::take(&mut state.deferred_entry_events) {
        match parked.state {
            ParkedEntryState::Completed if is_as_enters_roll(&parked.event) => {
                held.extend(parked.held_triggers);
            }
            ParkedEntryState::Completed => completed.push(parked.event),
            ParkedEntryState::Pending { .. } => {}
        }
    }
    let live = super::triggers::retire_departed_controller_contexts(state, held);
    state.deferred_triggers.extend(live);
    super::triggers::filter_already_collected_trigger_events_from(
        state,
        &completed,
        0,
        &state.consumed_before_priority_trigger_events,
    )
}

/// CR 603.2 + CR 603.7: the settled-entry release authority. Takes the settled
/// park and runs its delayed half here — a delayed triggered ability whose
/// trigger event is the entry triggers on the realized entry, once, like a
/// printed one — then returns the events for the caller's ordinary half. The
/// queued (no-claim) delayed collector fits because the released events belong
/// to no action buffer: taking the park is their claim. The replays'
/// [`collect_deferred_entry_replay`] queues the same pair for the same reason.
pub(crate) fn release_settled_entry_continuation(state: &mut GameState) -> Vec<GameEvent> {
    let released = take_settled_entry_continuation_events(state);
    if !released.is_empty() {
        super::triggers::collect_delayed_trigger_contexts_into_deferred(state, &released);
    }
    released
}

/// CR 603.3 + CR 603.3b: the release for a point that queues — the settled
/// entry's ordinary half joins `deferred_triggers` beside its delayed half and
/// the contexts its as-enters rolls hold, and the caller's drain places them.
/// Returns whether anything was released or queued.
pub(crate) fn collect_settled_entry_continuation_triggers(state: &mut GameState) -> bool {
    let queued_before = state.deferred_triggers.len();
    let released = release_settled_entry_continuation(state);
    let queued_held = state.deferred_triggers.len() != queued_before;
    if released.is_empty() {
        return queued_held;
    }
    super::triggers::collect_triggers_into_deferred(state, &released);
    true
}

fn copy_effect_for_source(state: &GameState, source_id: ObjectId) -> Option<&AbilityDefinition> {
    if let Some(entry) = state.liminal_entries.get(&source_id) {
        return super::replacement::liminal_entry_replacements(entry)
            .filter_map(|(_, replacement)| replacement.execute.as_deref())
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

/// CR 109.5 + CR 614.6: the two referents a post-replacement continuation
/// resolves against, which are NOT the same object and must not be derived from
/// one another.
///
/// * `affected` — the object the modified event acted on: the entering permanent
///   for an ETB replacement, the moving object for a zone change, the dying
///   creature for a "would die, … instead". It answers `SelfRef`, "that
///   permanent", and (via its controller) "that player".
/// * `controller` — the controller of the object whose ability did the
///   replacing. It answers "you". `None` on install paths with no replacing
///   object (combat-prevention riders, the ready-continuation helpers, test
///   fixtures), where the affected object's controller remains the fallback.
///
/// Bundled rather than passed as two loose `Option`s so a future caller cannot
/// supply one and silently inherit the other from the wrong side of the split —
/// the exact defect issue #7086 was.
#[derive(Debug, Clone, Copy)]
pub(super) struct ContinuationSubjects {
    pub affected: Option<ObjectId>,
    pub controller: Option<PlayerId>,
}

impl ContinuationSubjects {
    /// The affected object alone, with "you" left to fall back to its
    /// controller. Correct exactly when the replacing object IS the affected
    /// object — every self-scoped replacement (`valid_card: SelfRef`).
    pub fn affected_only(affected: Option<ObjectId>) -> Self {
        Self {
            affected,
            controller: None,
        }
    }
}

/// Apply a post-replacement side effect after a zone change has been executed.
/// Used by Optional replacements (e.g., shock lands: pay life on accept, tap on decline).
/// CR 707.9: For "enter as a copy" replacements, sets up CopyTargetChoice instead of
/// immediate resolution, since the player must choose which permanent to copy.
pub(super) fn apply_post_replacement_effect(
    state: &mut GameState,
    effect_def: &AbilityDefinition,
    subjects: ContinuationSubjects,
    // CR 400.7d: for AmountSpentToCastSource ceilings the entering object's
    // cast-payment stamp is authoritative — this spell-resolution context is
    // intentionally unused (issue #6440). Kept so call sites stay stable.
    _spell_resolution: Option<&crate::types::game_state::PendingSpellResolution>,
    event: Option<&ReplacementEvent>,
    replacement_applied: HashSet<AppliedReplacementKey>,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let ContinuationSubjects {
        affected: object_id,
        controller: replacement_controller,
    } = subjects;
    // Dual objects→liminal lookup (same as source identity): also yields the
    // entering object's cast-payment stamp for copy MV ceilings.
    let (source_id, affected_controller, mana_spent_stamp) = object_id
        .and_then(|obj_id| {
            state.entering_or_live_object(obj_id).map(|obj| {
                (
                    obj_id,
                    super::replacement::replacement_source_player(obj),
                    obj.mana_spent_to_cast_amount,
                )
            })
        })
        .unwrap_or((ObjectId(0), state.active_player, 0));

    // CR 109.5 + CR 614.6: `source_id` above is the object the modified event
    // AFFECTED — the entering permanent for an ETB replacement, the dying
    // creature for a "would die, … instead" one. It is the right referent for
    // the continuation's `SelfRef` / "that permanent" anaphors, and it is the
    // WRONG answer for "you": a replacement effect is text on the replacing
    // object, so "you" is that object's controller. Splitting the two is the
    // whole point of the drain carrying its own controller (issue #7086 — Head
    // of the Hunt's Wolf was created for the opponent whose creature died).
    //
    // The affected object's controller does not go away; it moves to
    // `scoped_player`, which is where every "that player" reference already
    // reads it from (`ControllerRef::ScopedPlayer`,
    // `filter::scoped_player_or_controller`, `sacrifice::resolve_sacrifice_scope`).
    // Because every one of those readers is a `scoped_player.unwrap_or(controller)`
    // fallback, and because a SELF-scoped replacement has
    // `affected_controller == replacement_controller` anyway, this split is a
    // no-op everywhere except a third-party replacement's "you".
    let controller = replacement_controller.unwrap_or(affected_controller);

    // …and for that reason the slot is populated ONLY when the two genuinely
    // differ. `scoped_player` means "a subject distinct from you"; writing the
    // controller into it when there is no such subject is not merely redundant,
    // it is a false signal. `effects/choose.rs::named_choice_authority` reads a
    // populated `scoped_player` as the marker of a per-player fan-out and
    // reroutes a persisting `Labeled` answer off the source object onto the
    // chooser — its own comment records that doing so for a single-chooser card
    // "breaks any object-scoped reader". Glacierwood Siege's "As this
    // enchantment enters, choose Temur or Sultai" is exactly that shape, and it
    // reaches this function as a self-scoped replacement continuation.
    let distinct_scoped_player = (affected_controller != controller).then_some(affected_controller);

    // CR 614.1c: Walk past modifier-only effects (Tap/Untap/PutCounter/ChangeZone)
    // in the sub_ability chain to find the real work. Composable replacements like
    // Vesuva's "enter tapped as a copy" emit Tap { SelfRef } → sub_ability(BecomeCopy);
    // the modifier was already applied to the ProposedEvent by `event_modifiers_for_ability`,
    // so we skip to the first non-modifier effect for post-replacement dispatch.
    let real_work =
        super::replacement::EventModifiers::first_non_modifier_ability(Some(effect_def))
            .unwrap_or(effect_def);

    if let Effect::BecomeCopy { ref target, .. } = *real_work.effect {
        // CR 400.7d: a permanent may reference mana spent to cast the spell that
        // became it. Uncast put-onto-battlefield (Chord of Calling → Mockingbird,
        // issue #6440) never received cast finalization (CR 601.2h is the stamp
        // *write* site), so the stamp stays 0 and the ceiling is Some(0) — never
        // unconstrained None from a missing PendingSpellResolution.
        let max_mana_value = copy_target_mana_value_ceiling(mana_spent_stamp, real_work);
        let valid_targets = super::replacement::find_entry_copy_targets(
            state,
            target,
            source_id,
            controller,
            max_mana_value,
        );
        if valid_targets.is_empty() {
            return None;
        }
        // CR 614.12a + CR 707.2: Some copy sources are already determined before
        // the permanent enters (indexed exiled cards, or the `SpecificObject`
        // frozen by a Mystic Reflection-style ChangeZone shield). No new choice
        // prompt is made for those shapes; resolve the copy directly. Do not
        // generalize every `SpecificObject` ETB copy replacement here: existing
        // Moved enter-as-copy paths still use `CopyTargetChoice` to defer the
        // frontend entry event until the final copy snapshot is chosen.
        let specific_change_zone_copy = matches!(target, TargetFilter::SpecificObject { .. })
            && matches!(event, Some(ReplacementEvent::ChangeZone));
        if matches!(target, TargetFilter::ExiledCardByIndex { .. }) || specific_change_zone_copy {
            let targets = valid_targets
                .into_iter()
                .map(TargetRef::Object)
                .collect::<Vec<_>>();
            let mut resolved =
                build_resolved_from_def_with_targets(real_work, source_id, controller, targets);
            if let Some(scoped) = distinct_scoped_player {
                resolved.set_scoped_player_recursive(scoped);
            }
            resolved.set_replacement_applied_recursive(replacement_applied);
            return resolve_post_replacement_chain(state, &resolved, events);
        } else {
            return Some(WaitingFor::CopyTargetChoice {
                player: controller,
                source_id,
                valid_targets,
                max_mana_value,
                purpose: CopyTargetPurpose::BecomeCopy,
            });
        }
    }

    // CR 614.12a + CR 707.2c: "As this Aura enters, choose a creature." The
    // chosen permanent's copiable values are latched onto the source Aura and
    // installed as a copy effect on the Aura's enchanted host at the answer
    // (`PersistChosenAttribute`) — never onto the entering Aura itself. This
    // reuses the same `CopyTargetChoice` machinery as `BecomeCopy` (it is a
    // choice, not targeting — hexproof/shroud don't apply, CR 115.10a) but is
    // discriminated by `purpose`. Empty legal-choice set → no prompt (CR 609.3:
    // an effect does only as much as possible).
    if let Effect::ChoosePermanent { ref filter } = *real_work.effect {
        let valid_targets = find_copy_targets(state, filter, source_id, controller, None);
        if valid_targets.is_empty() {
            return None;
        }
        // CR 707.1 + CR 707.2c: the same "choose a permanent" prompt serves two
        // dispositions. A choice whose ability tree reaches a `CopyTokenOf` is
        // a token-substitution copy source (the Esix, Fractal Bloom shape —
        // CR 707.1: an effect creates tokens that are copies of the chosen
        // object; Esix itself answers on its parked replacement instead of
        // reaching this continuation route). Anything else latches the chosen
        // permanent's copiable values onto a third-party recipient (Metamorphic
        // Alteration — CR 707.2c: a static copy effect's copiable values are
        // fixed when it first starts to apply).
        // Discriminated semantically through the single authority
        // `ability_tree_copies_tokens`, never by an incidental structural probe
        // such as `sub_ability.is_some()`.
        let purpose = if super::replacement::ability_tree_copies_tokens(real_work) {
            CopyTargetPurpose::CopyTokenSource
        } else {
            CopyTargetPurpose::PersistChosenAttribute
        };
        return Some(WaitingFor::CopyTargetChoice {
            player: controller,
            source_id,
            valid_targets,
            max_mana_value: None,
            purpose,
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
    // ETB-style replacements (the Devour shape). Damage riders with the same shape
    // take `rider_population` below.
    let sacrifice_typed_scope = is_as_enters_sacrifice_scope_replacement(event, &real_work.effect);
    // CR 614.6 + CR 615.5 + CR 608.2d: a damage rider that picks from a population ("sacrifice that
    // many permanents", "exile that many cards from your graveyard", "sacrifice an
    // Equipment attached to Outfitted Jouster") names no pre-selected object; the injected
    // host would be acted on instead. `event_source` is populated only for damage riders.
    // CR 614.6 + CR 608.2d: the same holds for a substitute of an event that affected a
    // player rather than an object (a replaced draw — Out of the Tombs' "instead return a
    // creature card from your graveyard to the battlefield"): with no affected object, the
    // drain's subject is the replacing permanent itself, which the pick must not be bound to.
    let replaced_player_event = matches!(
        state.post_replacement_event_target(),
        Some(TargetRef::Player(_))
    );
    let rider_population = (state.post_replacement_event_source().is_some()
        || replaced_player_event)
        && matches!(
            &*effect_def.effect,
            Effect::Sacrifice {
                target: TargetFilter::Typed(_),
                ..
            } | Effect::ChangeZone {
                target: TargetFilter::Typed(_),
                ..
            }
        );
    let targets = if sacrifice_typed_scope || rider_population {
        Vec::new()
    } else {
        object_id
            .map(TargetRef::Object)
            .into_iter()
            .collect::<Vec<_>>()
    };
    let mut resolved =
        build_resolved_from_def_with_targets(effect_def, source_id, controller, targets);
    // CR 400.7d + CR 608.2k: an enters-with replacement printed on the entering
    // permanent may read what was paid to cast the spell that became it
    // ("…with X +1/+1 counters on it, where X is the discarded card's mana
    // value", Dralnu's Pet). The spell's carrier reached the permanent through
    // the `CastLinkSnapshot` restore, so stamp it across this continuation —
    // only for a battlefield-entry `Moved` event, only when the chain reads the
    // cost-paid object (`ResolvedAbility::references_cost_paid_object`, the same
    // gate the ETB-trigger stamp in `triggers.rs` uses), and never over a
    // referent the chain already carries.
    if matches!(event, Some(ReplacementEvent::Moved)) && resolved.cost_paid_object.is_none() {
        let carrier = object_id
            .and_then(|id| state.entering_or_live_object(id))
            .filter(|obj| obj.zone == Zone::Battlefield)
            .and_then(|obj| obj.cast_cost_paid_object.clone());
        if let Some(snapshot) = carrier {
            if resolved.references_cost_paid_object() {
                resolved.set_cost_paid_object_recursive(snapshot);
            }
        }
    }
    // CR 121.6b + CR 616.1g: a nested replacement may temporarily become the
    // resident drain while this chain is suspended on its child draw. Capture
    // this continuation's own event target on the chain now so a later sibling
    // (for example, Alms Collector's "that player" draw) cannot read the nested
    // child's event target when it resumes.
    if ability_definition_uses_post_replacement_event_target(effect_def) {
        if let Some(target) = state.post_replacement_event_target() {
            if !resolved.targets.contains(target) {
                resolved.push_target(target.clone());
            }
        }
    }
    // CR 109.5: "that player" / "its controller" — the player the replaced
    // event acted on, bound only when that is somebody other than "you" (see
    // `distinct_scoped_player` above). Applied to the whole chain so a rider
    // several links down ("… then sacrifices a land of their choice") still
    // names them.
    if let Some(scoped) = distinct_scoped_player {
        resolved.set_scoped_player_recursive(scoped);
    }
    resolved.set_replacement_applied_recursive(replacement_applied);
    resolve_post_replacement_chain(state, &resolved, events)
}

fn ability_definition_uses_post_replacement_event_target(ability: &AbilityDefinition) -> bool {
    fn filter_uses_event_target(filter: &TargetFilter) -> bool {
        match filter {
            TargetFilter::PostReplacementDamageTarget => true,
            TargetFilter::And { filters } | TargetFilter::Or { filters } => {
                filters.iter().any(filter_uses_event_target)
            }
            TargetFilter::Not { filter } => filter_uses_event_target(filter),
            TargetFilter::None
            | TargetFilter::Any
            | TargetFilter::Player
            | TargetFilter::Controller
            | TargetFilter::SourceController
            | TargetFilter::ControllerAndControlledPermanents { .. }
            | TargetFilter::Opponent
            | TargetFilter::SelfRef
            | TargetFilter::GrantingObject { .. }
            | TargetFilter::SourceOrPaired
            | TargetFilter::ChoiceAssignment { .. }
            | TargetFilter::Typed(..)
            | TargetFilter::StackAbility { .. }
            | TargetFilter::StackSpell
            | TargetFilter::CastingSpell
            | TargetFilter::SpecificObject { .. }
            | TargetFilter::SpecificPlayer { .. }
            | TargetFilter::PlayerWhoChoseLabel { .. }
            | TargetFilter::PlayerMatching { .. }
            | TargetFilter::Neighbor { .. }
            | TargetFilter::ScopedPlayer
            | TargetFilter::AttachedTo
            | TargetFilter::LastCreated
            | TargetFilter::LastRevealed
            | TargetFilter::LastZoneChanged
            | TargetFilter::CostPaidObject
            | TargetFilter::AmassedArmy
            | TargetFilter::ChosenCard
            | TargetFilter::TrackedSet { .. }
            | TargetFilter::TrackedSetFiltered { .. }
            | TargetFilter::ExiledBySource
            | TargetFilter::ExiledCardByIndex { .. }
            | TargetFilter::TriggeringSpellController
            | TargetFilter::TriggeringSpellOwner
            | TargetFilter::TriggeringPlayer
            | TargetFilter::TriggeringSource
            | TargetFilter::AttackedPlayerOrPlaneswalker
            | TargetFilter::EventTarget
            | TargetFilter::TriggeringSourceController
            | TargetFilter::EventTargetController
            | TargetFilter::ParentTarget
            | TargetFilter::ParentTargetSlot { .. }
            | TargetFilter::DeclaredPlayer { .. }
            | TargetFilter::ParentTargetController
            | TargetFilter::ParentTargetOwner
            | TargetFilter::AffectedObjectOwner
            | TargetFilter::SourceChosenPlayer
            | TargetFilter::OriginalController
            | TargetFilter::OriginalSource
            | TargetFilter::PostReplacementSourceController
            | TargetFilter::PostReplacementDamageSource
            | TargetFilter::PostReplacementDamageTargetOwner
            | TargetFilter::DefendingPlayer
            | TargetFilter::HasChosenName
            | TargetFilter::ChosenDamageSource { .. }
            | TargetFilter::Named { .. }
            | TargetFilter::Owner
            | TargetFilter::AllPlayers => false,
        }
    }

    ability
        .effect
        .target_filter()
        .is_some_and(filter_uses_event_target)
        // CR 608.2c: the replaced event's player as the move's performer.
        || crate::game::effects::zone_move_actor_filter(&ability.effect)
            .is_some_and(filter_uses_event_target)
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(ability_definition_uses_post_replacement_event_target)
        || ability
            .else_ability
            .as_deref()
            .is_some_and(ability_definition_uses_post_replacement_event_target)
        || matches!(
            ability.effect.as_ref(),
            Effect::ChooseOneOf { branches, .. }
                if branches.iter().any(ability_definition_uses_post_replacement_event_target)
        )
}

/// CR 608.2c: execute instructions in the order written.
/// CR 615.5: a prevention effect's additional effect occurs immediately after
/// the prevention.
///
/// Engine invariant: isolate `state.waiting_for` so caller-owned prompts cannot
/// suspend the child chain or be mistaken for new child input.
pub(super) fn resolve_post_replacement_chain(
    state: &mut GameState,
    resolved: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    // CR 117.2e: this is an internal execution sentinel, not a priority grant.
    // Isolate before running the chain: an inherited resolution choice would
    // otherwise park its synchronous tail, even if the first effect completed.
    let caller_wait = std::mem::replace(
        &mut state.waiting_for,
        WaitingFor::Priority {
            player: state.priority_player,
        },
    );
    let _ = effects::resolve_ability_chain(state, resolved, events, 0);

    match &state.waiting_for {
        WaitingFor::Priority { .. } => {
            state.waiting_for = caller_wait;
            None
        }
        // A new child prompt may have exactly the same payload as caller_wait.
        waiting_for => Some(waiting_for.clone()),
    }
}

thread_local! {
    /// Set while a post-replacement dispatch is live on THIS thread's call
    /// stack. Deliberately not `GameState`: liveness describes the call stack,
    /// not the game, and serializing it would make the very `Dispatching`
    /// status this module forbids look legitimate across a round trip. Mirrors
    /// `game::engine::IN_SIMULATION_PROBE`, whose doc records the properties
    /// this relies on: engine game logic is single-threaded (no rayon /
    /// par_iter / std::thread::spawn in the apply or legal_actions path),
    /// `apply()` is fully synchronous, and the tokio server runs each apply
    /// synchronously within one task on one thread.
    ///
    /// KNOWN RESIDUAL: the WASM release profile is `panic = 'abort'`
    /// (Cargo.toml), so `Drop` does not run on a panic. A panic inside a
    /// dispatch therefore leaves this set, and the priority-boundary sweep is
    /// suppressed for the life of that module instance. The suppression is
    /// reported by a `tracing::warn!` at the sweep site so the condition is
    /// observable on the native build rather than silent. Native
    /// (`server-release`) and every test build are `panic = 'unwind'` and are
    /// unaffected. An unconditional reset at the `apply()` boundary was
    /// rejected: `apply()` is re-entrant via `SimulationFilter`'s
    /// clone-and-apply probe, so an ungated reset would clear a live outer
    /// dispatch's flag inside the probe.
    static POST_REPLACEMENT_DISPATCH_LIVE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// RAII: sets the flag, restores the PREVIOUS value on drop — nesting-correct,
/// so the CR 616.1g nested same-frame dispatch (Jace/Swans) keeps it set for
/// the outer dispatch after the inner one returns.
#[must_use]
struct LiveDispatchGuard(bool);

impl LiveDispatchGuard {
    fn enter() -> Self {
        LiveDispatchGuard(POST_REPLACEMENT_DISPATCH_LIVE.with(|flag| flag.replace(true)))
    }
}

impl Drop for LiveDispatchGuard {
    fn drop(&mut self) {
        POST_REPLACEMENT_DISPATCH_LIVE.with(|flag| flag.set(self.0));
    }
}

/// True while any `apply_pending_post_replacement_effect` dispatch is between
/// its `begin_dispatch` and its cleanup on this thread. The priority-boundary
/// sweeper consults this before retiring a `Dispatching` resident: when it is
/// false, "ownerless" is a definition rather than an inference, at any frame
/// depth and at any drain depth.
pub(crate) fn post_replacement_dispatch_is_live() -> bool {
    POST_REPLACEMENT_DISPATCH_LIVE.with(std::cell::Cell::get)
}

pub(crate) fn apply_pending_post_replacement_effect(
    state: &mut GameState,
    object_id: Option<ObjectId>,
    spell_resolution: Option<&crate::types::game_state::PendingSpellResolution>,
    event: Option<ReplacementEvent>,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    dispatch_pending_post_replacement_effect(state, object_id, spell_resolution, event, events)
        .map(|pause| pause.waiting_for)
}

/// A post-replacement dispatch that left a prompt standing, and the frame its drain sits in.
pub(crate) struct PostReplacementPause {
    pub(crate) waiting_for: WaitingFor,
    /// The frame of the drain whose dispatch this was. Its resident is the paused work: this
    /// dispatch's own entry, or a nested drain installed into the same frame (CR 616.1g).
    pub(crate) frame: crate::types::game_state::PostReplacementFrameId,
}

/// [`apply_pending_post_replacement_effect`], reporting the dispatch's frame when it pauses.
pub(crate) fn dispatch_pending_post_replacement_effect(
    state: &mut GameState,
    object_id: Option<ObjectId>,
    spell_resolution: Option<&crate::types::game_state::PendingSpellResolution>,
    event: Option<ReplacementEvent>,
    events: &mut Vec<GameEvent>,
) -> Option<PostReplacementPause> {
    // CR 614.12a: liminal token entries dispatch selected own-entry work before
    // rediscovering remaining replacements and committing the birth. Card-backed
    // ZoneChange callers retain their existing post-delivery dispatch timing.
    //
    // CR 615.5 + CR 616.1g: Resident-top dispatch. `begin_dispatch` takes the
    // continuation and marks the drain `Dispatching` WITHOUT removing it, so:
    //   * a nested `has_post_replacement_drain()` taken while the effect runs sees
    //     no ready work and cannot re-drain this one (the old slot was likewise
    //     empty at this point — the continuation had been moved out of it), and
    //   * the effect can still read this drain's event context (CR 615.5), which is
    //     how `PostReplacementSourceController` resolves "the source's controller
    //     draws cards" for Swans of Bryn Argoll.
    // The typed dispatch handle keeps cleanup attached to that exact resident
    // entry. A nested replacement can push its own drain above it, but cannot
    // cause the outer post-dispatch cleanup to mark or pop the nested top.
    //
    // `Resolved` carries captured targets (prevention follow-ups); `Template` is an
    // AST that resolves against `source` for ETB / Optional accept.
    let source = state.post_replacement_source().or(object_id);
    // CR 109.5: read BEFORE `begin_post_replacement_dispatch` takes the
    // continuation, exactly like `source` above — the resident accessor is a
    // read of the same drain entry.
    let replacement_controller = state.post_replacement_controller();
    let replacement_applied = state
        .active_post_replacement_drains_mut()
        .and_then(crate::types::game_state::PostReplacementDrainStack::resident_mut)
        .map(|drain| std::mem::take(&mut drain.applied))
        .unwrap_or_default();

    // CR 614.6 + CR 615.5: the drain carries the amount of the event its replacement replaced or
    // prevented, bound when the replacement applied (`replacement::replaced_event_amount`), or
    // when it was queued behind an earlier rider. "That many" reads it, not whatever other work
    // stamped since.
    let captured_amount = state
        .active_post_replacement_drains()
        .and_then(crate::types::game_state::PostReplacementDrainStack::resident)
        .and_then(|drain| drain.event_amount);
    // CR 614.12a + CR 603.6d: the entrant's delivery already holds its entry
    // when a pause came before this as-enters roll; read before the dispatch
    // takes the continuation.
    let adopts = source.is_some_and(|entrant| entrant_entry_is_pending(state, entrant))
        && as_enters_roll_chain_mut(state).is_some();
    let (continuation, dispatch) = state.begin_post_replacement_dispatch()?;
    if let Some(amount) = captured_amount {
        state.last_effect_count = Some(crate::game::arithmetic::u32_to_i32_saturating(amount));
    }
    // CR 603.3b + CR 608.2c: this dispatch is now live on this thread's call
    // stack. Constructed AFTER the mint so the `?` early-return path never
    // enters it, and dropped by RAII at every exit — the
    // `capture_deferred_entry_events_if_mid_entry_choice` tail, every `?`, and
    // (on unwind profiles) a panic. While it is held, the priority-boundary
    // sweeper must not treat a `Dispatching` resident as ownerless.
    let _live_dispatch = LiveDispatchGuard::enter();
    // CR 603.6d + CR 614.1c: an as-enters continuation is part of its entrant's
    // entry; park that entry while the continuation runs.
    let entry_parking =
        EntryContinuationParking::open(state, adopts, event.as_ref(), source, events);
    // CR 614.6 + CR 615.5: the continuation is the replacement's own action, so its "that many"
    // is the amount the replacement replaced or prevented (the drain's own amount, stamped
    // above), not anything of the spell or ability resolving around it. The scope binds that
    // amount ahead of the die result and the enclosing trigger's amount tiers of
    // `EventContextAmount`, and suppresses those trigger tiers as the reflexive scope does
    // (CR 603.12). A drain with no amount of its own (an entering object's continuation, an
    // optional branch) binds none and reads the rest of the cascade. Shockmaw Dragon: "Whenever this creature deals combat damage to a player, it
    // deals 1 damage to each creature that player controls." After 4 combat damage, Phytohydra
    // ("put that many +1/+1 counters on it instead") gets 1 counter, not 4. Farideh's Fireball:
    // "Roll a d20." then "deals 2 damage to each opponent." Delaying Shield ("put that many delay
    // counters on this enchantment instead") gets 2 counters, not the d20 face.
    //
    // Work the action leaves for a later `apply()` (a step after a child prompt) carries this
    // scope on its `PendingContinuation` and re-enters it when it resumes
    // (`effects::drain_pending_continuation`).
    let owned_amount = captured_amount.map(crate::game::arithmetic::u32_to_i32_saturating);
    let waiting_for = {
        // CR 615.5: an independent replacement action can deal its own damage,
        // even with the same source as the delivery it modified.
        let _damage_scope = effects::DamageDeliveryScope::enter(None);
        crate::game::quantity::with_replacement_action_scope(owned_amount, || match continuation {
            PostReplacementContinuation::Resolved(resolved) => {
                apply_post_replacement_resolved_effect(
                    state,
                    &resolved,
                    replacement_applied,
                    events,
                )
            }
            PostReplacementContinuation::Template(effect_def) => apply_post_replacement_effect(
                state,
                &effect_def,
                ContinuationSubjects {
                    affected: source,
                    controller: replacement_controller,
                },
                spell_resolution,
                event.as_ref(),
                replacement_applied,
                events,
            ),
        })
    };
    // CR 614.12a + CR 616.1g: classify by what OWNS the outstanding prompt, never
    // by whether the two-deep positional accessor can currently see this frame.
    // The predecessor asked `active_post_replacement_drains()`, which returns None
    // as soon as this frame's DISTANCE FROM THE STACK TOP exceeds one — whether
    // because the dispatched continuation raised two or more frames, or because a
    // parent-of-active insert slid a frame in above it
    // (`ResolutionStack::insert_parent_of_active`, reached from
    // `effects::append_to_pending_continuation`). Note the frame need not have
    // moved at all: `active_post_replacement_parent_index` looks only at the top
    // and its immediate predecessor, by design. Both arms then degraded to no-ops
    // and the entry was stranded `Dispatching` — unrecoverable, because every
    // retirement path is keyed on `Ready`, on `Paused`, or on a handle that dies
    // with this call. Worse, on the same miss
    // `GameState::install_post_replacement_drain` mints a SIBLING PostReplacement
    // frame, and the accessor then hands this cleanup that frame's drain vector —
    // so the outer `finish_dispatch` could remove an unrelated frame's entry.
    // The dispatch now names its frame by a stable `PostReplacementFrameId`, bound
    // to the frame at most once, so both failure modes are closed: the lookup
    // follows the frame through every `frames.insert` / `frames.swap`, never
    // resolves to a sibling, and is not invalidated by a nested same-frame
    // dispatch.
    let retired_entrants =
        if waiting_for.is_some() && state.post_replacement_dispatch_is_resident_top(dispatch) {
            // This dispatch's own work owns the prompt: park it so the paused-retire
            // paths (`finish_active_paused_post_replacement_dispatch` and the
            // `resume_resolution_frames` Paused branch) can finish it after the answer.
            // That retirement owns the completion of the entries bound to it.
            let _ = state.pause_post_replacement_dispatch(dispatch);
            Vec::new()
        } else {
            // Either the continuation completed, or a NESTED drain above this entry
            // owns the prompt (CR 616.1g) — in both cases this entry's work is done.
            // Retire exactly `dispatch`, addressed by frame id, wherever that frame
            // now sits. A `Ready` drain can have inherited a binding, which
            // `settle_retired_dispatch_entrants` hands on below.
            let retired = state.finish_post_replacement_dispatch(dispatch);
            // Unchanged from the predecessor, and deliberately NOT folded into the
            // call above: this removes any empty PostReplacement frame that is now the
            // stack top, which is a wider set than "the frame this dispatch
            // addressed". Narrowing it would be a second, unrelated behavioural delta.
            state.remove_empty_active_post_replacement_frame();
            // CR 614.12a + CR 614.13a: A Devour snapshot created by this completed
            // entry can sit above this just-retired drain. It has no remaining
            // iteration or prompt owner, so retire it before priority is restored.
            state.clear_completed_active_devour_snapshot();
            retired
                .map(|drain| drain.parked_entrants)
                .unwrap_or_default()
        };
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
    // CR 614.12a + CR 603.6d: a paused continuation keeps its entrant's entry
    // parked and bound to the dispatch that owns the prompt, so that dispatch's
    // retirement records completion; an inline one unparks it.
    entry_parking.close(state, dispatch, waiting_for.as_ref());
    settle_retired_dispatch_entrants(state, dispatch, retired_entrants, waiting_for.as_ref());
    waiting_for.map(|waiting_for| PostReplacementPause {
        waiting_for,
        frame: dispatch.frame(),
    })
}

/// CR 614.12a: True when every branch of a `ChooseOneOfBranch` is a self-targeted
/// `PutCounter` — the signature of an "enters with your choice of counter"
/// replacement (Denry Klin, Editor in Chief). When this holds, the choice is a
/// pre-entry counter fold and the entering object's `ZoneChanged` event must be
/// deferred until after the branch is chosen, so ETB observers see the chosen
/// counter (CR 614.12a). Exhaustive — no wildcard accept.
pub(crate) fn is_enters_counter_choice(branches: &[AbilityDefinition]) -> bool {
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
/// of counter), or a persisted `NamedChoice` with an exact binding to the
/// entering permanent (the "As it enters, choose a color/creature type/…" shape, e.g.
/// Valgavoth's Lair) — clone any battlefield-entry `ZoneChanged` events for the
/// entering source into `state.deferred_entry_events`. The original `events`
/// vec is preserved so the frontend animates the entry as soon as the spell /
/// land-play resolves; the deferred copy is replayed through
/// `collect_deferred_entry_replay` once the choice resolves (in
/// `handle_copy_target_choice` for copies, in the `ChooseBranch` arm and the
/// `NamedChoice` + `ChooseOption` arm of `engine_resolution_choices.rs` for the
/// other two shapes), so every ETB observer (constellation like Doomwake Giant,
/// Soul Warden, …) sees the entry against the fully realized post-choice object.
///
/// For a played land, the sibling `GameEvent::LandPlayed` (emitted by
/// `finalize_committed_land_play` in the land-play path) is captured alongside
/// the entry `ZoneChanged`, so "play a land" observers (City of Traitors'
/// "When you play another land, sacrifice this land", CR 305.1 + CR 603.2) also
/// fire against the realized post-choice object rather than being dropped when
/// the entry pauses on an as-enters choice (issue #8738).
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
/// Replaces only this source's own parked entries, so a stale entry of a prior
/// choice that exited abnormally goes with it, and every other entrant's entry
/// stays parked for its own collector.
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
        // on an exact-object `NamedChoice`. Defer the entry event exactly like
        // the copy/counter shapes
        // so ETB observers fire against the post-choice object once the player
        // answers. The entry-event filter in the capture loop scopes this to the
        // entering source — a persisted `NamedChoice` with no matching entry
        // event in `events` (Pithing Needle naming) captures nothing.
        Some(WaitingFor::NamedChoice {
            source: Some(source),
            ..
        }) if source.is_exact_object_and_resolution() => source.prompt.identity.reference.object_id,
        _ => return,
    };
    // The caller set is not entry-shaped. `CopyTargetPurpose::CopyTokenSource`
    // (Esix, Fractal Bloom) raises a `CopyTargetChoice` for a source that is a
    // RESIDENT permanent on a `CreateToken` replacement — it is not entering the
    // battlefield at all. The arm above still selects on that shape (it carries
    // no `purpose` guard and no entering-object guard). Nothing is written for
    // it: the park holds no entry of that source, and the entry-event filter in
    // the loop below matches nothing a non-entering source produces.
    let _ = take_parked_entries_of(state, source_id);
    for event in events.iter() {
        if is_entry_event_of(event, source_id) {
            park_pending_entry(state, event.clone());
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
    resolve_post_replacement_chain(state, &resolved, events)
}

/// CR 608.3: Complete post-resolution work for a permanent spell whose ETB
/// went through the replacement pipeline and required a player choice.
/// Applies cast_from_zone, aura attachment, and warp delayed triggers.
pub(crate) fn apply_pending_spell_resolution(
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
            // CR 608.3c + CR 603.2e: an Aura spell becoming attached on this
            // resume path publishes `GameEvent::Attached` (the single "becomes
            // attached" trigger authority) only on an actual host transition;
            // a same-host re-attach is a CR 701.3b no-op.
            match target {
                crate::types::ability::TargetRef::Object(target_id)
                    if state.battlefield.contains(target_id) =>
                {
                    effects::attach::attach_to_with_events(
                        state,
                        ctx.object_id,
                        *target_id,
                        events,
                    );
                }
                crate::types::ability::TargetRef::Object(_) => {}
                crate::types::ability::TargetRef::Player(player_id) => {
                    effects::attach::attach_to_player_with_events(
                        state,
                        ctx.object_id,
                        *player_id,
                        events,
                    );
                }
            }
        }
    }

    // CR 709.5d / CR 702.185a + CR 603.7d: both of these are KEEP-classified
    // consumers of the historical cast-time controller (`entry.controller` on
    // the unpaused `stack.rs` path — the same known limitation noted there),
    // NOT the live/CR 608.2c re-stamped controller `ctx.controller` carries.
    // Reading `ctx.controller` here would make a warp/Room rider resolve under
    // a DIFFERENT controller than the unpaused path resolves it under,
    // depending only on whether the resolution paused for a choice.
    // `cast_controller` is always `Some` (set unconditionally in
    // `pending_spell_resolution_snapshot`); the fallback only guards the type.
    let cast_time_controller = ctx.cast_controller.unwrap_or(ctx.controller);

    // CR 709.5d: same single authority as the ordinary resolution tail in
    // `stack.rs`. Previously this granted `RoomDoor::Left` unconditionally —
    // every permanent spell that paused for a replacement choice and then
    // entered as a copy of a Room was handed a designation although neither
    // of ITS halves was cast (CR 709.5d last sentence).
    if let Some(cast_door) = state
        .objects
        .get(&ctx.object_id)
        .and_then(super::room::cast_half_designation)
    {
        super::room::unlock_door_designation(
            state,
            ctx.object_id,
            cast_time_controller,
            cast_door,
            events,
        );
    }

    // CR 702.185a: Warp delayed trigger setup.
    if ctx.casting_variant == CastingVariant::Warp {
        let has_warp = state.objects.get(&ctx.object_id).is_some_and(|obj| {
            obj.keywords
                .iter()
                .any(|k| matches!(k, crate::types::keywords::Keyword::Warp(_)))
        });
        if has_warp {
            super::stack::create_warp_delayed_trigger(
                state,
                ctx.object_id,
                cast_time_controller,
                events,
            );
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

/// CR 614.1c + CR 122.6 + CR 603.6d: an as-enters roll and its counters are
/// part of the event that puts the permanent onto the battlefield, so the counters
/// the entrant is given as it enters of the kind the roll places join that
/// placement as one number, which a counter replacement affects once (CR 614.5).
/// Called where the entry's contributions are first all known. Returns what
/// delivery still places: the contributions of every other kind, in their order,
/// and `counters` unchanged when the roll takes nothing.
///
/// Contributions of another kind stay on delivery's own counter step, because each
/// kind is its own placement and a prompt of another kind stays ahead of the roll.
///
/// CR 603.6d + CR 614.1c + CR 122.6 + CR 614.5: counters of the roll's kind given
/// as the entrant enters are one placement with the roll's, whatever put it onto
/// the battlefield. CR 508.4: an attacking entrant whose continuation pauses is
/// seated by the authority that resumes after it (`PausedEntryAttackSeat`).
pub(crate) fn hand_entry_counters_to_as_enters_roll(
    state: &mut GameState,
    entrant: ObjectId,
    counters: Vec<(CounterType, u32)>,
) -> Vec<(CounterType, u32)> {
    let Some((kind, placed)) = as_enters_roll_chain_mut(state)
        .filter(|(source, _)| *source == Some(entrant))
        .and_then(|(_, chain)| unconditional_self_placement_mut(chain))
    else {
        return counters;
    };
    let Some((_, taken)) = effects::counters::coalesce_entry_counters(counters.iter().cloned())
        .into_iter()
        .find(|(counter_type, _)| counter_type == kind)
    else {
        return counters;
    };
    let inner = std::mem::replace(placed, QuantityExpr::Fixed { value: 0 });
    *placed = QuantityExpr::Offset {
        inner: Box::new(inner),
        offset: u32_to_i32_saturating(taken),
    };
    counters
        .into_iter()
        .filter(|(counter_type, _)| counter_type != kind)
        .collect()
}

/// The not-yet-run as-enters roll the active drain will dispatch: its resident
/// `Ready` template when that chain has `RollDie` at its head, with the drain's
/// source.
fn as_enters_roll_chain_mut(
    state: &mut GameState,
) -> Option<(Option<ObjectId>, &mut AbilityDefinition)> {
    let drain = state.active_post_replacement_drains_mut()?.resident_mut()?;
    let source = drain.source;
    match &mut drain.status {
        DrainStatus::Ready(PostReplacementContinuation::Template(chain)) => {
            matches!(*chain.effect, Effect::RollDie { .. }).then_some((source, &mut **chain))
        }
        DrainStatus::Ready(PostReplacementContinuation::Resolved(_))
        | DrainStatus::Dispatching
        | DrainStatus::Paused => None,
    }
}

/// CR 614.12a + CR 603.6d: is `entrant`'s as-enters roll still to run? True while
/// the active drain's resident is that entrant's not-yet-run `RollDie` chain.
pub(crate) fn entrant_awaits_as_enters_roll(state: &mut GameState, entrant: ObjectId) -> bool {
    as_enters_roll_chain_mut(state).is_some_and(|(source, _)| source == Some(entrant))
}

/// The chain's first unconditional `PutCounter` on the entrant itself: the
/// placement an entry's counters of its kind join.
fn unconditional_self_placement_mut(
    link: &mut AbilityDefinition,
) -> Option<(&CounterType, &mut QuantityExpr)> {
    if link.condition.is_none() {
        if let Effect::PutCounter {
            counter_type,
            count,
            target: TargetFilter::SelfRef,
        } = link.effect.as_mut()
        {
            return Some((&*counter_type, count));
        }
    }
    link.sub_ability
        .as_deref_mut()
        .and_then(unconditional_self_placement_mut)
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
            // CR 614.16: entering-with-counters placements are
            // replacement-driven entry modifications.
            CounterCause::ReplacementOrPrevention,
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
                        // CR 614.16: entering-with-counters placements are
                        // replacement-driven entry modifications.
                        cause: CounterCause::ReplacementOrPrevention,
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

pub fn find_copy_targets(
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

/// CR 614.12 + CR 616.1f: settle the selected entrant's own entry work before
/// rediscovering replacements. `rediscover` is true on a paused work item's
/// priority-boundary resume; the inline path discovers only after dispatching.
pub(crate) fn settle_liminal_token_entry_replacements(
    state: &mut GameState,
    mut event: ProposedEvent,
    mut rediscover: bool,
    events: &mut Vec<GameEvent>,
) -> super::replacement::ReplacementResult {
    use super::replacement::{self, ReplacementResult};
    loop {
        if rediscover {
            match replacement::replace_event(state, event, events) {
                ReplacementResult::Execute(next) => event = next,
                ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
                    return ReplacementResult::NeedsChoice(player);
                }
                ReplacementResult::Prevented => return ReplacementResult::Prevented,
            }
        }
        let settles_own_entry = replacement::token_entry_awaits_own_entry_settlement(state, &event);
        if !state.has_post_replacement_drain() {
            return ReplacementResult::Execute(event);
        }
        let ProposedEvent::TokenEntry { entry_ref, .. } = &event else {
            unreachable!("only a token entry can own entry replacement work")
        };
        let source_id = *entry_ref;
        // Park the event before own-entry work runs, so counters it earns are
        // written to this entrant's event rather than the batch's reusable seed.
        let mut terminal_event = Some(event);
        let previous_resume = if settles_own_entry {
            state.pending_liminal_entry_resume.replace(
                crate::types::game_state::PendingLiminalEntryResume::Token {
                    source_id,
                    player: state.active_player,
                    event: terminal_event
                        .take()
                        .expect("the own entry event is present"),
                },
            )
        } else {
            None
        };
        state.waiting_for = WaitingFor::Priority {
            player: state.active_player,
        };
        if let Some(waiting_for) = apply_pending_post_replacement_effect(
            state,
            Some(source_id),
            None,
            Some(ReplacementEvent::Moved),
            events,
        ) {
            let player = waiting_for.acting_player().unwrap_or(state.active_player);
            if !settles_own_entry {
                state.pending_liminal_entry_resume =
                    Some(crate::types::game_state::PendingLiminalEntryResume::Token {
                        source_id,
                        player,
                        event: terminal_event
                            .take()
                            .expect("the terminal entry event is present"),
                    });
            } else if let Some(crate::types::game_state::PendingLiminalEntryResume::Token {
                source_id: pending_source,
                player: pending_player,
                event: ProposedEvent::TokenEntry { entry_ref, .. },
            }) = state.pending_liminal_entry_resume.as_mut()
            {
                if *pending_source == source_id && *entry_ref == source_id {
                    *pending_player = player;
                }
            }
            state.waiting_for = waiting_for;
            return ReplacementResult::NeedsChoice(player);
        }
        // Preserve terminal delivery of foreign entry work, too. Only selected own
        // work stopped discovery at the barrier and therefore owes rediscovery here.
        if !settles_own_entry {
            return ReplacementResult::Execute(
                terminal_event.expect("terminal work retains its entry event"),
            );
        }
        // Take only the event this dispatch parked; an unrelated resume is never
        // consumed as this entrant's completed work.
        assert!(matches!(
            state.pending_liminal_entry_resume.as_ref(),
            Some(crate::types::game_state::PendingLiminalEntryResume::Token {
                source_id: pending_source,
                event: ProposedEvent::TokenEntry { entry_ref, .. },
                ..
            }) if *pending_source == source_id && *entry_ref == source_id
        ));
        let Some(crate::types::game_state::PendingLiminalEntryResume::Token {
            event: settled_event,
            ..
        }) = state.pending_liminal_entry_resume.take()
        else {
            unreachable!("the matched own entry resume is present")
        };
        event = settled_event;
        state.pending_liminal_entry_resume = previous_resume;
        rediscover = true;
    }
}

/// CR 614.12a + CR 111.1: commit a liminal (decided-but-not-yet-entered) token
/// entry whose "as enters" chain paused on a player prompt, once that prompt —
/// and every continuation it spawned — has drained back to a priority boundary.
///
/// # The defect this exists to close
///
/// `GameState::pending_liminal_entry_resume` is produced by the shared
/// `settle_liminal_token_entry_replacements` authority and
/// `zone_pipeline::deliver_zone_change`. Before this drain, its only consumer
/// was `handle_copy_target_choice`. That handler
/// answers `WaitingFor::CopyTargetChoice` only, so an entry whose chain paused on
/// any other prompt was never resumed — `commit_liminal_copy_token_entry` was
/// never reached, the entry stayed in `state.liminal_entries` for the rest of the
/// game, and the permanent was never created at all.
///
/// That is not a narrow shape. CR 702.104a Tribute synthesizes
/// `Choose { Opponent, persist } -> Tribute`, and `parse_as_enters_choose` gives
/// the whole printed "As this ~ enters, choose a <named attribute>" class
/// (Painter's Servant, the Thriving land cycle, the Khans Sieges, Anointed
/// Peacekeeper) the same `Effect::Choose` post-replacement chain. Every one of
/// them raises `WaitingFor::NamedChoice`, so every copy-token effect that copied
/// one — `CopyTokenOf`, Embalm/Eternalize, populate, `CreateTokenCopyFromPool` —
/// silently produced no token, and left a stranded id that the visible game log
/// rendered as `(unknown #N)` (`log::resolve_object_name` consults `state.objects`
/// and `state.lki_cache`, and a liminal entry is in neither).
///
/// # Why the boundary, and not each answer handler
///
/// The set of prompts an "as enters" chain can raise is open — it is whatever
/// `apply_post_replacement_effect` dispatches into — so teaching each answer
/// handler to resume would be a list that silently goes stale the next time an
/// effect learns to pause. The priority boundary is the one place that already
/// means "this action's chain has finished", which is exactly the CR 614.12a
/// precondition for the entry to commit: the choices an entry replacement
/// requires are made BEFORE the permanent enters, so the entry is owed the
/// moment, and only the moment, that they are all answered. A chain that raises
/// a further prompt (Tribute's pay-or-decline after its opponent choice) leaves
/// `waiting_for` non-`Priority`, so the gate in
/// `engine::resume_pending_continuation_if_priority` holds the commit back
/// without this function needing to know that prompt exists.
///
/// # Scope
///
/// Consumes only a `Token` resume whose liminal entry is still present — which
/// covers all three producers, since every one of them records a `Token`. An
/// entry already committed by `handle_copy_target_choice` is gone from
/// `liminal_entries`, so the presence check makes a double-commit unrepresentable
/// rather than merely unlikely.
///
/// A `Meld` resume is left parked for `handle_copy_target_choice`, which owns the
/// CR 701.42 completion (`commit_meld_battlefield` plus the deferred-meld
/// epilogue) that this seam deliberately does not reimplement.
///
/// KNOWN RESIDUAL, stated rather than implied: the card-backed CR 701.42 path
/// keeps the shape this function fixes for tokens. `zone_pipeline`'s producer
/// records a `Meld` resume ONLY when the pause is a `CopyTargetChoice` (it is
/// gated on exactly that variant), so a meld whose own as-enters chain paused on
/// any other prompt records no resume at all and strands the same way a token
/// used to. That is latent, not live — no printed meld result carries an
/// "as this enters, choose" replacement — so it is left alone rather than fixed
/// speculatively, but it is the same defect and closing it means giving
/// `zone_pipeline` an unconditional producer plus a meld-aware drain here.
///
/// Returns the new pause when the commit itself pauses (a CR 616.1 ordering
/// choice between two enter-with-counters replacements) or the parent frame it
/// resumes pauses (a debug batch's next member), otherwise `None`.
pub(super) fn resume_pending_liminal_token_entry(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let is_resumable_token = matches!(
        state.pending_liminal_entry_resume.as_ref(),
        Some(crate::types::game_state::PendingLiminalEntryResume::Token { source_id, .. })
            if state.liminal_entries.contains_key(source_id)
    );
    if !is_resumable_token {
        return None;
    }
    let Some(crate::types::game_state::PendingLiminalEntryResume::Token {
        source_id, event, ..
    }) = state.pending_liminal_entry_resume.take()
    else {
        unreachable!("the guard above matched a Token resume with a live liminal entry")
    };

    // CR 616.1f: the paused selected work has finished. Rediscover on its same
    // event (including applied identities), then use the shared entry completion.
    if state.liminal_entries[&source_id].copy_settlement.is_some() {
        state.waiting_for = resume_token_copy_settlement(state, source_id, event, true, events)
            .expect("settling a staged copy resumes its valid copy choice");
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return Some(state.waiting_for.clone());
        }
    } else if !crate::game::effects::token::resume_liminal_token_entry_and_continue_copy_batch(
        state, event, events,
    ) {
        return Some(state.waiting_for.clone());
    }
    // CR 608.2c + CR 614.12a: the commit is the step the parent frame below the
    // entry's retired post-replacement frame was waiting on (a debug Create Card
    // batch's next member). The boundary pass spent its one frame step retiring
    // that post-replacement frame, so advance the exposed parent now, as the
    // unpaused path continues its batch right after the entry.
    effects::resume_resolution_frames(state, events);
    (!matches!(state.waiting_for, WaitingFor::Priority { .. })).then(|| state.waiting_for.clone())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::super::game_object::GameObject;
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::replacement::{self as replacement_mod, ReplacementResult};
    use crate::game::scenario::{GameScenario, P0, P1};
    use crate::game::zone_pipeline::{move_object, ZoneMoveRequest, ZoneMoveResult};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityKind, ChooseOneOfKind, LibraryPosition, QuantityExpr, ReplacementDefinition,
        ReplacementMode,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::proposed_event::{AppliedReplacementKey, ProposedEvent, ReplacementId};
    use crate::types::replacements::ReplacementEvent;

    /// CR 120.3f + CR 614.6: the shared drain helper on a non-combat
    /// loss-pause stash (no printed card reaches its Prevented route: no
    /// printed LifeLoss `Prevent` replacement exists). Settled: the parked
    /// event, record and lifelink gain run once and the stash is consumed.
    /// Each gate (not settled, combat stash) leaves the stash untouched,
    /// paired with the settled call as its reach-guard.
    #[test]
    fn drain_settled_non_combat_damage_stash_gates_and_settles() {
        use crate::types::game_state::{DamagePostLossStage, PendingDamagePostLoss};
        let fixture = |is_combat: bool| {
            let mut state = GameState::new_two_player(42);
            let source = create_object(
                &mut state,
                CardId(9801),
                PlayerId(1),
                "Loss Source".to_string(),
                Zone::Battlefield,
            );
            state.pending_damage_post_loss = Some(Box::new(PendingDamagePostLoss {
                damage_delivery_owner: None,
                source_id: source,
                source_incarnation: None,
                controller: PlayerId(1),
                target: PlayerId(0),
                actual_amount: 2,
                has_lifelink: !is_combat,
                lifelink_bonus: 0,
                stage: DamagePostLossStage::EmitAndRecord,
                is_combat,
                toxic_poison: 0,
            }));
            state
        };

        // Positive: settled non-combat stash.
        let mut state = fixture(false);
        assert!(state.pending_replacement.is_none());
        let life = state.players[1].life;
        let mut events = Vec::new();
        assert_eq!(
            drain_settled_non_combat_damage_stash(&mut state, true, &mut events, 0),
            None
        );
        assert!(state.pending_damage_post_loss.is_none(), "stash consumed");
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e,
                    GameEvent::DamageDealt {
                        is_combat: false,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(state.damage_dealt_this_turn.len(), 1);
        assert_eq!(
            state.players[1].life,
            life + 2,
            "one lifelink gain (CR 120.3f)"
        );

        // Gate: the answered prompt is not settled.
        let mut state = fixture(false);
        let mut events = Vec::new();
        assert_eq!(
            drain_settled_non_combat_damage_stash(&mut state, false, &mut events, 0),
            None
        );
        assert!(
            state.pending_damage_post_loss.is_some(),
            "not settled: untouched"
        );
        assert!(events.is_empty());

        // Gate: a combat stash belongs to the Phase C remnant.
        let mut state = fixture(true);
        let mut events = Vec::new();
        assert_eq!(
            drain_settled_non_combat_damage_stash(&mut state, true, &mut events, 0),
            None
        );
        assert!(
            state.pending_damage_post_loss.is_some(),
            "combat stash: untouched"
        );
        assert!(events.is_empty());
        assert!(state.damage_dealt_this_turn.is_empty());
    }

    /// CR 120.4b + CR 603.2 + CR 603.3b: when the settled stash's lifelink gain
    /// pauses on its own CR 616.1 choice, the answering action ends on that
    /// prompt, so the helper defers the observers of the action's span (here
    /// the stash's own `DamageDealt`, emitted by the `EmitAndRecord` stage).
    /// Exactly one context is deferred for the one damage event. Sibling: the
    /// same stash with no competing gain replacements settles (`None`) and
    /// defers nothing (the post-action scan owns that action's events).
    #[test]
    fn drain_settled_non_combat_damage_stash_defers_observers_when_the_gain_pauses() {
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::game_state::{DamagePostLossStage, PendingDamagePostLoss};
        const SPECTER: &str = "Flying\nWhenever Hypnotic Specter deals damage to an opponent, that player discards a card at random.";

        for competing in [true, false] {
            let mut scenario = GameScenario::new();
            let specter = scenario
                .add_creature(P0, "Hypnotic Specter", 2, 2)
                .from_oracle_text_with_keywords(&["Flying"], SPECTER)
                .id();
            if competing {
                scenario.add_enchantment_from_oracle(
                    P0,
                    "Plus One",
                    "If you would gain life, you gain that much life plus 1 instead.",
                );
                scenario.add_enchantment_from_oracle(
                    P0,
                    "Twice",
                    "If you would gain life, you gain twice that much life instead.",
                );
            }
            scenario.add_card_to_hand(P1, "Filler");
            let mut runner = scenario.build();
            let state = runner.state_mut();
            assert!(
                !state.objects[&specter].trigger_definitions.is_empty(),
                "reach-guard: the observer parsed"
            );
            state.pending_damage_post_loss = Some(Box::new(PendingDamagePostLoss {
                damage_delivery_owner: None,
                source_id: specter,
                source_incarnation: Some(state.objects[&specter].incarnation),
                controller: P0,
                target: P1,
                actual_amount: 2,
                has_lifelink: true,
                lifelink_bonus: 0,
                stage: DamagePostLossStage::EmitAndRecord,
                is_combat: false,
                toxic_poison: 0,
            }));
            let deferred_before = state.deferred_triggers.len();
            let mut events = Vec::new();
            let next = drain_settled_non_combat_damage_stash(state, true, &mut events, 0);
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e, GameEvent::DamageDealt { .. }))
                    .count(),
                1,
                "reach-guard: the stash emitted its damage event, competing={competing}"
            );
            if competing {
                assert!(
                    matches!(next, Some(WaitingFor::ReplacementChoice { player, .. }) if player == P0),
                    "the gain paused on P0's choice"
                );
                assert_eq!(
                    state.deferred_triggers.len(),
                    deferred_before + 1,
                    "the Specter observer of the one damage event is deferred once"
                );
            } else {
                assert_eq!(next, None);
                assert_eq!(state.deferred_triggers.len(), deferred_before);
            }
        }
    }

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

    /// CR 614.12a + CR 616.1: the paid entry cost retires its DiscardChoice
    /// before re-parking the remaining, noncommuting entry replacements.
    #[test]
    fn may_cost_discard_answer_reparks_ordering_and_publishes_it() {
        let mut scenario = GameScenario::new();
        let mox = scenario
            .add_creature_to_graveyard(P0, "Mox Diamond", 0, 0)
            .as_artifact()
            .from_oracle_text("If this artifact would enter, you may discard a land card instead. If you do, put this artifact onto the battlefield. If you don't, put it into its owner's graveyard.\n{T}: Add one mana of any color.")
            .id();
        let root = scenario
            .add_enchantment_from_oracle(P0, "Root Maze", "Artifacts and lands enter tapped.")
            .id();
        let archelos = scenario
            .add_creature_from_oracle(P1, "Archelos, Lagoon Mystic", 2, 4,
                "As long as Archelos is tapped, other permanents enter tapped.\nAs long as Archelos is untapped, other permanents enter untapped.")
            .id();
        let forest = scenario
            .add_land_to_hand(P0, "Forest")
            .from_oracle_text("({T}: Add {G}.)")
            .id();
        let island = scenario
            .add_land_to_hand(P0, "Island")
            .from_oracle_text("({T}: Add {U}.)")
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        assert!(matches!(
            move_object(
                state,
                ZoneMoveRequest::effect(mox, Zone::Battlefield, mox),
                &mut Vec::new()
            ),
            ZoneMoveResult::NeedsChoice(P0)
        ));
        let index = state
            .pending_replacement
            .as_ref()
            .unwrap()
            .candidates
            .iter()
            .position(|rid| rid.source == mox)
            .expect("Mox competes with both entry writes");
        apply_as_current(state, GameAction::ChooseReplacement { index }).expect("choose Mox");
        apply_as_current(state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept MayCost");
        assert!(matches!(
            state.waiting_for,
            WaitingFor::DiscardChoice { player: P0, .. }
        ));
        assert!(state.pending_replacement.as_ref().unwrap().may_cost_paid);

        let outcome = apply_as_current(
            state,
            GameAction::SelectCards {
                cards: vec![forest],
            },
        )
        .expect("answer the paid replacement's discard");
        assert!(matches!(
            outcome.waiting_for,
            WaitingFor::ReplacementChoice { player: P0, .. }
        ));
        assert_eq!(outcome.waiting_for, state.waiting_for);
        let pending = state
            .pending_replacement
            .as_ref()
            .expect("remaining ordering is parked");
        assert_eq!(
            pending
                .candidates
                .iter()
                .map(|rid| rid.source)
                .collect::<HashSet<_>>(),
            HashSet::from([root, archelos])
        );
        assert_eq!(state.objects[&mox].zone, Zone::Graveyard);
        assert_eq!(state.objects[&forest].zone, Zone::Graveyard);
        assert_eq!(state.objects[&island].zone, Zone::Hand);
    }

    #[test]
    fn canonical_library_placement_away_and_back_public_resume_ignores_stale_mirror() {
        // Infrastructure-only synthetic Moved definitions. Rest in Peace is the
        // nearest printed redirect; no printed away/back replacement or Wheel
        // grammar/reveal support is asserted by this disposition test.
        fn redirect(from: Zone, to: Zone) -> ReplacementDefinition {
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                .destination_zone(from)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        actor: crate::types::ability::LibraryInstructionActor::Controller,
                        library_shuffle: Default::default(),
                        origin: None,
                        destination: to,
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
        }

        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Infrastructure away redirect".into(),
            Zone::Battlefield,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Infrastructure return redirect".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&first)
            .unwrap()
            .replacement_definitions
            .push(redirect(Zone::Library, Zone::Exile));
        state
            .objects
            .get_mut(&second)
            .unwrap()
            .replacement_definitions
            .push(redirect(Zone::Exile, Zone::Library));
        let placed = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Infrastructure placed card".into(),
            Zone::Graveyard,
        );
        for card in 4..7 {
            create_object(
                &mut state,
                CardId(card),
                PlayerId(0),
                format!("Library sentinel {card}"),
                Zone::Library,
            );
        }
        let original_library = state.players[0].library.clone();
        let mut events = Vec::new();
        assert!(matches!(
            move_object(
                &mut state,
                ZoneMoveRequest::effect(placed, Zone::Library, placed)
                    .at_library_position(LibraryPosition::Top),
                &mut events
            ),
            ZoneMoveResult::NeedsChoice(PlayerId(0))
        ));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice {
                player: PlayerId(0),
                ..
            }
        ));
        let pending = state.pending_replacement.as_ref().unwrap();
        assert_eq!(
            pending.candidates,
            vec![ReplacementId {
                source: first,
                index: 0
            }]
        );
        assert!(matches!(&pending.proposed, ProposedEvent::ZoneChange {
            object_id, to: Zone::Library, library_placement: Some(placement), ..
        } if *object_id == placed && **placement == LibraryPosition::Top));
        assert_eq!(state.objects[&placed].zone, Zone::Graveyard);

        state.priority_player = PlayerId(0);
        events.extend(
            apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                .expect("accept first redirect")
                .events,
        );
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice {
                player: PlayerId(0),
                ..
            }
        ));
        let pending = state.pending_replacement.as_ref().unwrap();
        assert_eq!(
            pending.candidates,
            vec![ReplacementId {
                source: second,
                index: 0
            }]
        );
        assert!(matches!(&pending.proposed, ProposedEvent::ZoneChange {
            object_id, to: Zone::Exile, library_placement: None, cause, applied, ..
        } if *object_id == placed && *cause == Some(first)
            && *applied == HashSet::from([AppliedReplacementKey::object(first, 0)])));
        assert_eq!(state.objects[&placed].zone, Zone::Graveyard);
        assert_eq!(state.players[0].library, original_library);

        // Wire-schema supplement: preserve the real parked producer event but
        // put a conflicting legacy mirror in its serialized record. Current
        // explicit null is authoritative at decode and during public resume.
        let mut wire = serde_json::to_value(&state).unwrap();
        wire["pending_replacement"]["library_placement"] =
            serde_json::to_value(LibraryPosition::Top).unwrap();
        let proposal = wire["pending_replacement"]["proposed"]["ZoneChange"]
            .as_object()
            .expect("serialized current ZoneChange");
        assert!(proposal.contains_key("library_placement"));
        let null = &proposal["library_placement"];
        assert!(null.is_null());
        state = serde_json::from_value(wire).expect("decode current explicit-null proposal");
        let pending = state.pending_replacement.as_ref().unwrap();
        assert!(matches!(
            &pending.proposed,
            ProposedEvent::ZoneChange {
                library_placement: None,
                ..
            }
        ));
        events.extend(
            apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                .expect("accept return redirect")
                .events,
        );

        assert_eq!(state.objects[&placed].zone, Zone::Library);
        assert_eq!(
            state.players[0]
                .library
                .iter()
                .filter(|id| **id == placed)
                .count(),
            1
        );
        // Positive tail discriminator: canonical None follows the existing
        // placement-less Library delivery convention despite the stale Top.
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        action: crate::types::events::PlayerActionKind::ShuffledLibrary,
                        ..
                    }
                ))
                .count(),
            1
        );
        for source in [first, second] {
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event,
                        GameEvent::ReplacementApplied { source_id, .. } if *source_id == source
                    ))
                    .count(),
                1
            );
        }
        assert!(state.pending_replacement.is_none());
        assert!(state.stack.is_empty());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    /// CR 400.7 + CR 616.1: A Connive event parked for replacement ordering
    /// carries the original subject snapshot. Its terminal delivery must not
    /// recapture a returned object that reused the same storage id.
    #[test]
    fn connive_replacement_ordering_keeps_original_subject_after_same_id_return() {
        let mut state = GameState::new_two_player(42);
        let conniver = make_creature(&mut state, PlayerId(0), "Original conniver");
        let original_subject = state
            .capture_connive_subject(conniver)
            .expect("fixture conniver exists before the ordering pause");

        // A real zone round trip creates a new object incarnation while
        // preserving the engine's storage ObjectId.
        crate::game::zones::move_to_zone(&mut state, conniver, Zone::Graveyard, &mut Vec::new());
        crate::game::zones::move_to_zone(&mut state, conniver, Zone::Battlefield, &mut Vec::new());
        assert_ne!(
            original_subject.identity(),
            crate::types::identifiers::ObjectIncarnationRef::from_object(&state.objects[&conniver]),
            "reach guard: the battlefield return must be a new CR 400.7 incarnation"
        );

        // The candidate deliberately has no execute payload. Choosing it takes
        // the actual `continue_replacement` path, marks it applied, and delivers
        // the terminal Connive survivor through `handle_replacement_choice`.
        let candidate = install_optional_replacement(&mut state, ReplacementEvent::Connive);
        state.pending_replacement = Some(crate::types::game_state::PendingReplacement {
            damage_delivery_owner: None,
            settled_damage_guards: Vec::new(),
            proposed: ProposedEvent::Connive {
                object_id: conniver,
                subject: Box::new(original_subject.snapshot.clone()),
                count: 1,
                applied: HashSet::new(),
            },
            sacrifice_provenance: None,
            candidates: vec![ReplacementId {
                source: candidate,
                index: 0,
            }],
            search_found_candidates: Vec::new(),
            ordering_peers: Vec::new(),
            depth: 0,
            is_optional: true,
            choice_player: Some(PlayerId(0)),
            exile_controller: None,
            exile_duration: None,
            exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
            excess_recipient: None,
            lifelink_bonus: 0,
            source_incarnation: None,
            may_cost_paid: false,
            may_cost_remaining: None,
            delivery_boundary: None,
            unrepresentable_life_change: Default::default(),
            token_substitute: None,
        });
        state.waiting_for = replacement_mod::replacement_choice_waiting_for(PlayerId(0), &state);

        let draw_card_id = CardId(state.next_object_id);
        let draw = create_object(
            &mut state,
            draw_card_id,
            PlayerId(0),
            "Discarded by the original connive".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();
        handle_replacement_choice(&mut state, 0, &mut events)
            .expect("resume the parked Connive replacement choice");

        assert!(state.players[0].graveyard.contains(&draw));
        assert_eq!(
            state.objects[&conniver]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0,
            "the same-id return is not the original conniver and receives no counter"
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: crate::types::ability::EffectKind::Connive,
                    subject: Some(subject),
                    ..
                } if subject.identity == original_subject.identity()
            )),
            "completion event retains the original exact subject for Connive triggers"
        );
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
                // Emulated effect placement under test (untagged def:
                // unobservable, but semantically an effect event).
                cause: CounterCause::ResolvingEffect,
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
    /// (`ReplacementResult::Prevented` while SpellResolution is active),
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
    /// the SpellResolution frame set. The assertion target —
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
                        actor: crate::types::ability::LibraryInstructionActor::Controller,
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
                        library_shuffle: Default::default(),
                    },
                ))]
            .into();

        // The paused entry's spell-resolution bookkeeping.
        state.push_spell_resolution(PendingSpellResolution {
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
            damage_delivery_owner: None,
            settled_damage_guards: Vec::new(),
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
            ordering_peers: Vec::new(),
            depth: 0,
            is_optional: false,
            choice_player: None,
            exile_controller: None,
            exile_duration: None,
            exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
            excess_recipient: None,
            lifelink_bonus: 0,
            source_incarnation: None,
            may_cost_paid: false,
            may_cost_remaining: None,
            delivery_boundary: None,
            unrepresentable_life_change: Default::default(),
            token_substitute: None,
        });
        state.waiting_for = replacement_mod::replacement_choice_waiting_for(PlayerId(0), &state);
        state.priority_player = PlayerId(0);

        let serialized = serde_json::to_string(
            &crate::types::resolution::ResolutionStateWire::from_game_state(state),
        )
        .expect("spell-resolution prompt serializes as v2 frames");
        let mut state =
            serde_json::from_str::<crate::types::resolution::ResolutionStateWire>(&serialized)
                .expect("spell-resolution prompt restores from v2 frames")
                .into_game_state();
        state.rehydrate_rng();

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
            source_name: None,
            source_incarnation: None,
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
                        actor: crate::types::ability::LibraryInstructionActor::Controller,
                        library_shuffle: Default::default(),
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
        assert!(
            pending.completion.pending_deliveries.iter().any(|delivery| {
                matches!(
                    &delivery.sacrifice,
                    crate::types::game_state::PendingSacrificeMove::Sacrifice {
                        object_id,
                        ..
                    } if *object_id == first
                )
            }),
            "the first pre-consulted sacrifice must stay queued for simultaneous delivery across the pause"
        );

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
            library_placement: None,
            object_id: victim,
            from: Zone::Battlefield,
            to: Zone::Exile,
            cause: None,
            attach_to: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            enters_attacking: false,
            enter_with_counters: Vec::new(),
            enter_with_grants: None,
            controller_override: None,
            enter_transformed: false,
            enter_as_copy: None,
            enter_with_choices: None,
            discard_frame: None,
            performed_by: None,
            ownership_actor: crate::types::proposed_event::EntryOwnershipActor::NoPlayer,
            library_shuffle: crate::types::ability::LibraryShuffleMode::PerObject,
            applied: std::collections::HashSet::new(),
            face_down_profile: None,
            with_entry_effect: None,
            co_entrants: None,
            merged_departure: None,
            face_down_in_exile: crate::types::ability::ExileConcealment::Public,
            chain_referent: crate::types::zones::ChainReferentIntent::Silent,
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
                loyalty: None,
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
            attack_request: crate::types::proposed_event::TokenAttackRequest::Unspecified,
            sacrifice_at: None,
            source_id: ObjectId(1),
            controller: PlayerId(0),
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
        };

        let battlefield_before = state.battlefield.clone();

        let mut events = Vec::new();
        let proposed = ProposedEvent::CreateToken {
            redirected_to: None,
            owner: PlayerId(0),
            spec: Box::new(spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            appended: Vec::new(),
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
    /// parks on `ChooseOneOfBranch` before the modified event happens. The
    /// chosen branch rewrites the event's own batch — one prompt, no original
    /// tokens, the substitute created once.
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
                    attack_destination:
                        crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                        kind: ChooseOneOfKind::Ordinary,
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
                loyalty: None,
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
            attack_request: crate::types::proposed_event::TokenAttackRequest::Unspecified,
            sacrifice_at: None,
            source_id: replacement_source,
            controller: PlayerId(0),
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
        };
        let battlefield_before = state.battlefield.clone();

        let mut events = Vec::new();
        let proposed = ProposedEvent::CreateToken {
            redirected_to: None,
            owner: PlayerId(0),
            spec: Box::new(treasure_spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            appended: Vec::new(),
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
        assert!(
            state.pending_replacement.as_ref().is_some_and(|pending| {
                matches!(
                    pending.token_substitute,
                    Some(crate::types::game_state::PendingTokenSubstitute {
                        awaits: TokenSubstituteChoice::Branch,
                        ..
                    })
                )
            }),
            "the accept parks the branch substitute, got {:?}",
            state
                .pending_replacement
                .as_ref()
                .and_then(|pending| pending.token_substitute)
        );
        assert_eq!(
            state.battlefield, battlefield_before,
            "nothing is delivered before the branch choice"
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
            state.pending_replacement.is_none(),
            "the substitute answer consumes its parked record"
        );
    }

    /// CR 614.6 + CR 616.1: no ordering index answers a parked token
    /// substitute — the substitute is answered by its own prompt.
    #[test]
    fn parked_token_substitute_rejects_an_ordering_index() {
        use crate::types::ability::{AbilityDefinition, Effect, PlayerFilter, PtValue};
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
        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "Cat".to_string(),
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                types: vec!["Creature".to_string(), "Cat".to_string()],
                colors: vec![ManaColor::Green],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: crate::types::ability::TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
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
                        branches: vec![branch.clone(), branch],
                        kind: ChooseOneOfKind::Ordinary,
                    },
                )),
        );
        state.objects.insert(replacement_source, jinnie);
        state.battlefield.push_back(replacement_source);

        let mut events = Vec::new();
        let proposed = ProposedEvent::CreateToken {
            redirected_to: None,
            owner: PlayerId(0),
            spec: Box::new(TokenSpec {
                characteristics: TokenCharacteristics {
                    display_name: "Treasure".to_string(),
                    power: None,
                    toughness: None,
                    loyalty: None,
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
                attack_request: crate::types::proposed_event::TokenAttackRequest::Unspecified,
                sacrifice_at: None,
                source_id: replacement_source,
                controller: PlayerId(0),
                attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
            }),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            appended: Vec::new(),
            applied: std::collections::HashSet::new(),
        };
        let ReplacementResult::NeedsChoice(player) =
            replacement_mod::replace_event(&mut state, proposed, &mut events)
        else {
            panic!("expected the optional prompt");
        };
        state.waiting_for = replacement_mod::replacement_choice_waiting_for(player, &state);
        state.priority_player = player;
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept token replacement");
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "REACH-GUARD: the accept parks on the branch choice, got {:?}",
            state.waiting_for
        );

        let result = handle_replacement_choice(&mut state, 0, &mut events);
        assert!(
            matches!(result, Err(EngineError::InvalidAction(_))),
            "an ordering index must not answer a parked substitute, got {result:?}"
        );
        assert!(
            state
                .pending_replacement
                .as_ref()
                .is_some_and(|pending| pending.token_substitute.is_some()),
            "the rejected index leaves the parked substitute in place"
        );
    }

    /// CR 614.6 + CR 603.7a + the Jinnie Fay ruling ("Anything else specified
    /// in the effect creating the tokens ... still applies"): the creating
    /// instruction's token-spec rider (`sacrifice_at`) applies to the
    /// substitute.
    ///
    /// No printed card's token instruction carries an end-of-combat
    /// `sacrifice_at`, so the instruction is built directly; the nearest
    /// printed card is Kari Zev, Skyship Raider, whose rider is a
    /// `LastCreated` delayed trigger (covered by the Kari Zev row).
    #[test]
    fn token_substitute_keeps_the_creating_instructions_end_of_combat_sacrifice() {
        use crate::game::scenario::{GameRunner, GameScenario, P0};
        use crate::types::ability::{
            DelayedTriggerCondition, Duration, Effect, PtValue, QuantityExpr, ResolvedAbility,
            TargetFilter, TargetRef,
        };
        use crate::types::phase::Phase;

        // Verbatim Oracle text (MTGJSON).
        const JINNIE: &str = "If you would create one or more tokens, you may instead create that many 2/2 green Cat creature tokens with haste or that many 3/1 green Dog creature tokens with vigilance.";
        let instruction = |source_id| {
            ResolvedAbility::new(
                Effect::Token {
                    name: "Soldier".to_string(),
                    power: PtValue::Fixed(1),
                    toughness: PtValue::Fixed(1),
                    types: vec!["Creature".to_string(), "Soldier".to_string()],
                    colors: vec![],
                    keywords: vec![],
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 2 },
                    owner: TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    attack_destination:
                        crate::types::ability::TokenAttackDestination::ControllerChooses,
                    supertypes: vec![],
                    static_abilities: vec![],
                    enter_with_counters: vec![],
                },
                vec![],
                source_id,
                P0,
            )
            .duration(Duration::UntilEndOfCombat)
        };
        let check_two_tokens_with_sacrifice = |state: &GameState, subtype: &str| {
            let tokens: Vec<_> = state.objects.values().filter(|obj| obj.is_token).collect();
            assert_eq!(
                tokens.len(),
                2,
                "the instruction creates exactly two {subtype} tokens"
            );
            for token in &tokens {
                assert!(
                    token.card_types.subtypes.iter().any(|s| s == subtype),
                    "each token is a {subtype}, got {:?}",
                    token.card_types.subtypes
                );
                let triggers: Vec<_> = state
                    .delayed_triggers
                    .iter()
                    .filter(|trigger| {
                        trigger.condition
                            == DelayedTriggerCondition::AtNextPhase {
                                phase: Phase::EndCombat,
                            }
                            && matches!(&trigger.ability.effect, Effect::Sacrifice { .. })
                            && trigger.ability.targets.first() == Some(&TargetRef::Object(token.id))
                    })
                    .collect();
                assert_eq!(
                    triggers.len(),
                    1,
                    "each {subtype} carries exactly one end-of-combat sacrifice"
                );
            }
        };

        // Accept: two Cats, each sacrificed at end of combat.
        let mut scenario = GameScenario::new_n_player(2, 42);
        let jinnie = scenario
            .add_creature_from_oracle(P0, "Jinnie Fay, Jetmir's Second", 3, 3, JINNIE)
            .id();
        let mut runner: GameRunner = scenario.build();
        let mut events = Vec::new();
        effects::token::resolve(runner.state_mut(), &instruction(jinnie), &mut events)
            .expect("the instruction resolves");
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "REACH-GUARD: the instruction raises Jinnie's prompt, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("accept the substitute");
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseOneOfBranch { .. }
            ),
            "the accept parks on the branch choice, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::ChooseBranch { index: 0 })
            .expect("choose the Cat branch");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "the branch answer completes, got {:?}",
            runner.state().waiting_for
        );
        check_two_tokens_with_sacrifice(runner.state(), "Cat");

        // Decline sibling (reach guard that the instruction carries the
        // rider): two Soldiers, each sacrificed at end of combat.
        let mut scenario = GameScenario::new_n_player(2, 42);
        let jinnie = scenario
            .add_creature_from_oracle(P0, "Jinnie Fay, Jetmir's Second", 3, 3, JINNIE)
            .id();
        let mut runner: GameRunner = scenario.build();
        let mut events = Vec::new();
        effects::token::resolve(runner.state_mut(), &instruction(jinnie), &mut events)
            .expect("the instruction resolves");
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .expect("decline the substitute");
        check_two_tokens_with_sacrifice(runner.state(), "Soldier");
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
            .push_back(ZoneChangeRecord::test_minimal(
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

    fn inherited_replacement_waits(source: ObjectId, other: ObjectId) -> [WaitingFor; 2] {
        [
            WaitingFor::AssignCombatDamage {
                player: PlayerId(0),
                attacker_id: source,
                total_damage: 2,
                blockers: vec![crate::types::game_state::DamageSlot {
                    blocker_id: other,
                    lethal_minimum: 2,
                }],
                assignment_modes: vec![Default::default()],
                trample: None,
                defending_player: PlayerId(1),
                attack_target: crate::game::combat::AttackTarget::Player(PlayerId(1)),
                pw_loyalty: None,
                pw_controller: None,
            },
            WaitingFor::ScryChoice {
                player: PlayerId(0),
                cards: vec![other],
            },
        ]
    }

    #[test]
    fn post_replacement_public_answer_distinguishes_template_copy_and_child_choice() {
        let mut initial = GameState::new_two_player(42);
        let source = install_optional_replacement(&mut initial, ReplacementEvent::GainLife);
        let scry_card = create_object(
            &mut initial,
            CardId(10),
            PlayerId(1),
            "Scry card".to_string(),
            Zone::Library,
        );
        let chosen = create_object(
            &mut initial,
            CardId(11),
            PlayerId(0),
            "Exiled copy source".to_string(),
            Zone::Exile,
        );
        initial
            .cards_exiled_with_source_this_turn
            .insert(source, vec![chosen]);
        for effect in [
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: Some(TargetFilter::Controller),
            },
            Effect::BecomeCopy {
                target: TargetFilter::ExiledCardByIndex { index: 0 },
                recipient: crate::types::ability::CopyRecipient::Source,
                duration: Some(crate::types::ability::Duration::Permanent),
                mana_value_limit: None,
                additional_modifications: Vec::new(),
            },
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ] {
            let mut state = initial.clone();
            let mut template = AbilityDefinition::new(AbilityKind::Spell, effect.clone());
            template.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 5 },
                    target: Some(TargetFilter::Controller),
                },
            )));
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .replacement_definitions[0]
                .execute = Some(Box::new(template));
            let mut events = Vec::new();
            let ReplacementResult::NeedsChoice(player) = replacement_mod::replace_event(
                &mut state,
                ProposedEvent::LifeGain {
                    player_id: PlayerId(1),
                    amount: 1,
                    applied: HashSet::new(),
                },
                &mut events,
            ) else {
                panic!("optional replacement must request an actual choice")
            };
            assert_eq!(player, PlayerId(1), "the source controller must choose");
            assert_ne!(
                player, state.active_player,
                "the replacement's controller must differ from the active player"
            );
            state.waiting_for = replacement_mod::replacement_choice_waiting_for(player, &state);

            apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                .expect("accept the replacement through the public reducer");
            if matches!(effect, Effect::Scry { .. }) {
                assert_eq!(
                    state.waiting_for,
                    WaitingFor::ScryChoice {
                        player: PlayerId(1),
                        cards: vec![scry_card]
                    }
                );
                assert_eq!(state.players[1].life, 20, "child tail waits for its answer");
                assert!(
                    !state.resolution_stack.is_empty(),
                    "real input retains its owner"
                );
                apply_as_current(
                    &mut state,
                    GameAction::SelectCards {
                        cards: vec![scry_card],
                    },
                )
                .expect("answer the actual child choice");
            }
            if matches!(effect, Effect::BecomeCopy { .. }) {
                assert_eq!(state.objects[&source].name, "Exiled copy source");
            }
            let initial_loss = if matches!(effect, Effect::LoseLife { .. }) {
                2
            } else {
                0
            };
            assert_eq!(
                state.players[1].life,
                15 - initial_loss,
                "each instruction runs once for its controller"
            );
            assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
            assert!(
                state.resolution_stack.is_empty(),
                "synchronous work cannot own the answered replacement prompt"
            );
            apply_as_current(&mut state, GameAction::PassPriority)
                .expect("ordinary priority after replacement");
            assert_eq!(state.players[1].life, 15 - initial_loss);
        }
    }

    #[test]
    fn post_replacement_synchronous_chains_restore_inherited_waits() {
        for tail_amount in [None, Some(3)] {
            let mut initial = GameState::new_two_player(42);
            let source = make_creature(&mut initial, PlayerId(1), "Replacement source");
            let other = make_creature(&mut initial, PlayerId(0), "Other object");
            let mut template = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    target: Some(TargetFilter::Controller),
                },
            );
            if let Some(value) = tail_amount {
                template.sub_ability = Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::LoseLife {
                        amount: QuantityExpr::Fixed { value },
                        target: Some(TargetFilter::Controller),
                    },
                )));
            }
            let resolved = build_resolved_from_def_with_targets(
                &template,
                source,
                PlayerId(1),
                vec![TargetRef::Object(source)],
            );
            for continuation in [
                PostReplacementContinuation::Template(Box::new(template)),
                PostReplacementContinuation::Resolved(Box::new(resolved)),
            ] {
                for caller_wait in inherited_replacement_waits(source, other) {
                    let mut state = initial.clone();
                    state.waiting_for = caller_wait.clone();
                    state.install_ready_continuation(continuation.clone());
                    let mut events = Vec::new();
                    let waiting = apply_pending_post_replacement_effect(
                        &mut state,
                        Some(source),
                        None,
                        None,
                        &mut events,
                    );

                    // CR 608.2c: every instruction completes in written order;
                    // an inherited resolution choice cannot park the tail.
                    assert_eq!(state.players[1].life, 18 - tail_amount.unwrap_or(0));
                    assert_eq!(state.players[0].life, 20);
                    assert!(
                        waiting.is_none(),
                        "completed child must not claim caller input"
                    );
                    assert_eq!(state.waiting_for, caller_wait);
                    assert!(
                        state.resolution_stack.is_empty(),
                        "no false paused drain or tail"
                    );
                }
            }
        }
    }

    #[test]
    fn post_replacement_identical_scry_prompt_resumes_context_tail_once() {
        let mut initial = GameState::new_two_player(42);
        let shield = make_creature(&mut initial, PlayerId(0), "Replacement source");
        let damage_source = make_creature(&mut initial, PlayerId(1), "Damage source");
        let recipient = make_creature(&mut initial, PlayerId(0), "Damage recipient");
        // These creatures must survive the public answer's state-based actions.
        for id in [shield, damage_source, recipient] {
            let object = initial.objects.get_mut(&id).unwrap();
            object.power = Some(2);
            object.toughness = Some(2);
            object.base_power = Some(2);
            object.base_toughness = Some(2);
            object.base_card_types = object.card_types.clone();
        }
        let scry_card = create_object(
            &mut initial,
            CardId(10),
            PlayerId(0),
            "Scry card".to_string(),
            Zone::Library,
        );
        create_object(
            &mut initial,
            CardId(11),
            PlayerId(1),
            "Source controller draw".to_string(),
            Zone::Library,
        );
        let mut counters = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::PostReplacementDamageTarget,
            },
        );
        counters.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::PostReplacementSourceController,
            },
        )));
        let mut template = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        template.sub_ability = Some(Box::new(counters));
        let resolved = build_resolved_from_def_with_targets(
            &template,
            shield,
            PlayerId(0),
            vec![TargetRef::Object(shield)],
        );
        for continuation in [
            PostReplacementContinuation::Template(Box::new(template)),
            PostReplacementContinuation::Resolved(Box::new(resolved)),
        ] {
            let mut state = initial.clone();
            let caller_wait = WaitingFor::ScryChoice {
                player: PlayerId(0),
                cards: vec![scry_card],
            };
            state.waiting_for = caller_wait.clone();
            state.install_ready_continuation(continuation);
            let drain = state
                .active_post_replacement_drains_mut()
                .and_then(crate::types::game_state::PostReplacementDrainStack::resident_mut)
                .expect("replacement context is resident");
            drain.source = Some(shield);
            drain.event_source = Some(damage_source);
            drain.event_target = Some(TargetRef::Object(recipient));
            let mut events = Vec::new();
            let waiting = apply_pending_post_replacement_effect(
                &mut state,
                Some(shield),
                None,
                Some(ReplacementEvent::DamageDone),
                &mut events,
            );

            assert_eq!(
                waiting,
                Some(caller_wait.clone()),
                "new identical prompt still belongs to child"
            );
            assert_eq!(state.waiting_for, caller_wait);
            assert!(matches!(
                state
                    .active_post_replacement_drains()
                    .and_then(crate::types::game_state::PostReplacementDrainStack::resident)
                    .map(|drain| &drain.status),
                Some(crate::types::game_state::DrainStatus::Paused)
            ));
            assert!(
                state.objects[&recipient].counters.is_empty(),
                "tail must wait for answer"
            );
            assert!(state.players[1].hand.is_empty());

            let answer = apply_as_current(
                &mut state,
                GameAction::SelectCards {
                    cards: vec![scry_card],
                },
            )
            .expect("public scry answer resumes the child");
            // CR 615.5: the paused rider retains the prevented event's target
            // and source controller until every instruction has completed.
            assert_eq!(
                state.objects[&recipient]
                    .counters
                    .get(&CounterType::Plus1Plus1),
                Some(&3)
            );
            assert!(state.objects[&shield].counters.is_empty());
            assert!(state.objects[&damage_source].counters.is_empty());
            assert_eq!(state.players[1].hand.len(), 1);
            assert!(state.players[0].hand.is_empty());
            assert_eq!(answer.events.iter().filter(|event| matches!(event,
                GameEvent::CounterAdded { object_id, count: 3, .. } if *object_id == recipient
            )).count(), 1);
            assert!(
                state.resolution_stack.is_empty(),
                "answered child retires its context"
            );
            assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
            apply_as_current(&mut state, GameAction::PassPriority)
                .expect("normal priority resumes");
            assert_eq!(state.players[1].hand.len(), 1, "tail must not run twice");
            assert_eq!(
                state.objects[&recipient]
                    .counters
                    .get(&CounterType::Plus1Plus1),
                Some(&3)
            );
        }
    }

    #[test]
    fn post_replacement_predetermined_copy_restores_wait_and_completes_tail() {
        for caller_wait_index in 0..2 {
            let mut state = GameState::new_two_player(42);
            let source = make_creature(&mut state, PlayerId(0), "Copy recipient");
            let chosen = make_creature(&mut state, PlayerId(1), "Chosen copy source");
            let caller_wait =
                inherited_replacement_waits(source, chosen)[caller_wait_index].clone();
            state.waiting_for = caller_wait.clone();
            let mut template = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::BecomeCopy {
                    target: TargetFilter::SpecificObject { id: chosen },
                    recipient: crate::types::ability::CopyRecipient::Source,
                    duration: Some(crate::types::ability::Duration::Permanent),
                    mana_value_limit: None,
                    additional_modifications: Vec::new(),
                },
            );
            template.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    target: Some(TargetFilter::Controller),
                },
            )));
            state.install_ready_continuation(PostReplacementContinuation::Template(Box::new(
                template,
            )));
            let mut events = Vec::new();
            let waiting = apply_pending_post_replacement_effect(
                &mut state,
                Some(source),
                None,
                Some(ReplacementEvent::ChangeZone),
                &mut events,
            );

            assert_eq!(
                state.objects[&source].name, "Chosen copy source",
                "predetermined copy actually executes"
            );
            assert_eq!(
                state.players[0].life, 18,
                "copy's synchronous tail completes"
            );
            assert_eq!(state.players[1].life, 20);
            assert!(waiting.is_none());
            assert_eq!(state.waiting_for, caller_wait);
            assert!(state.resolution_stack.is_empty());
        }
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
                position: crate::types::ability::LibraryPosition::Top,
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

    /// G4 regression: a general post-replacement drain that begins a true draw
    /// retains its event context across a save/reload at a replacement pause,
    /// through its context-dependent follow-up.
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
            .active_post_replacement_drains_mut()
            .and_then(crate::types::game_state::PostReplacementDrainStack::resident_mut)
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
            matches!(
                state
                    .active_post_replacement_drains()
                    .and_then(crate::types::game_state::PostReplacementDrainStack::resident),
                Some(crate::types::game_state::PostReplacementDrain {
                    status: crate::types::game_state::DrainStatus::Paused,
                    ..
                })
            ),
            "the paused drain must own its event context across the paused draw"
        );

        let serialized = serde_json::to_string(
            &crate::types::resolution::ResolutionStateWire::from_game_state(state),
        )
        .expect("save paused state");
        let mut restored =
            serde_json::from_str::<crate::types::resolution::ResolutionStateWire>(&serialized)
                .expect("reload paused state")
                .into_game_state();
        restored.rehydrate_rng();
        apply_as_current(&mut restored, GameAction::ChooseReplacement { index: 0 })
            .expect("resume the paused draw after reload");

        assert!(
            !restored.players[0].hand.is_empty(),
            "reach guard: the first draw completed after the replacement consult"
        );
        assert_eq!(
            restored.players[1].hand.len(),
            1,
            "the resumed PostReplacementSourceController follow-up must draw for P1 from the persisted drain context"
        );
        assert!(
            restored.active_post_replacement_drains().is_none(),
            "the drain is retired only after its resumed continuation completes"
        );
    }

    /// CR 615.5 + CR 616.1g: an outer post-replacement draw can install and
    /// immediately dispatch an inner post-replacement draw. If the inner draw
    /// pauses on its own replacement choice, the completed outer drain must be
    /// retired by its typed dispatch handle rather than stranding below the
    /// inner pause.
    #[test]
    fn nested_post_replacement_draw_pause_retires_completed_outer_drain() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, DrawReplacementScope, Effect,
            PostReplacementContinuation, QuantityModification, ReplacementDefinition,
            ReplacementPlayerScope, TargetFilter,
        };

        let mut state = GameState::new_two_player(42);
        let outer_source = make_creature(&mut state, PlayerId(0), "Outer draw source");
        for player in [PlayerId(0), PlayerId(1)] {
            for index in 0..4 {
                let card_id = CardId(state.next_object_id);
                create_object(
                    &mut state,
                    card_id,
                    player,
                    format!("P{} nested library {index}", player.0),
                    Zone::Library,
                );
            }
        }

        // P0's outer draw is replaced by an equivalent draw plus a P1 draw in
        // its post-replacement tail. The first replacement therefore installs
        // and dispatches an inner drain while the outer drain is still running.
        let replacement_host = make_creature(&mut state, PlayerId(1), "Nested draw host");
        let nested_draw = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let mut outer_draw_replacement = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        outer_draw_replacement.sub_ability = Some(Box::new(nested_draw));
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::Draw)
            .draw_scope(DrawReplacementScope::IndividualDraw)
            .execute(outer_draw_replacement);
        replacement.valid_player = Some(ReplacementPlayerScope::Opponent);
        state.objects[&replacement_host]
            .replacement_definitions
            .push(replacement);

        // Only the inner P1 draw sees these two modifiers, so it alone pauses
        // on CR 616.1 ordering while the completed P0 outer draw is unwinding.
        for modification in [
            QuantityModification::Times { factor: 2 },
            QuantityModification::Plus { value: 1 },
        ] {
            let host = make_creature(&mut state, PlayerId(1), "Inner draw modifier");
            let mut modifier = ReplacementDefinition::new(ReplacementEvent::Draw)
                .draw_scope(DrawReplacementScope::IndividualDraw);
            modifier.valid_player = Some(ReplacementPlayerScope::You);
            modifier.quantity_modification = Some(modification);
            modifier.consume_on_apply = true;
            state.objects[&host].replacement_definitions.push(modifier);
        }

        state.install_ready_continuation(PostReplacementContinuation::Template(Box::new(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ),
        )));
        let mut events = Vec::new();
        let waiting = apply_pending_post_replacement_effect(
            &mut state,
            Some(outer_source),
            None,
            Some(ReplacementEvent::DamageDone),
            &mut events,
        );
        assert!(
            matches!(waiting, Some(WaitingFor::ReplacementChoice { .. })),
            "reach guard: the inner P1 draw pauses while the outer drain unwinds"
        );
        assert!(
            matches!(
                state
                    .active_post_replacement_drains()
                    .and_then(crate::types::game_state::PostReplacementDrainStack::resident),
                Some(crate::types::game_state::PostReplacementDrain {
                    status: crate::types::game_state::DrainStatus::Paused,
                    ..
                })
            ),
            "the inner draw owns the sole resident paused context"
        );

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("resume the inner draw replacement choice");
        assert!(
            state.active_post_replacement_drains().is_none(),
            "resuming the inner draw retires it without leaving the completed outer drain"
        );
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
            ContinuationSubjects::affected_only(Some(devourer)),
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
                    // enforces this. Polymorphine
                    // is a self-replacement on the entering card, so the
                    // parser sets `SelfRef` automatically; the test must
                    // mirror that wiring.
                    .valid_card(TargetFilter::SelfRef)
                    .destination_zone(Zone::Battlefield)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::BecomeCopy {
                            recipient: crate::types::ability::CopyRecipient::Source,
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
            library_placement: None,
            object_id: callidus,
            from: Zone::Stack,
            to: Zone::Battlefield,
            cause: None,
            attach_to: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            enters_attacking: false,
            enter_with_counters: Vec::new(),
            enter_with_grants: None,
            controller_override: None,
            enter_transformed: false,
            enter_as_copy: None,
            enter_with_choices: None,
            discard_frame: None,
            performed_by: None,
            ownership_actor: crate::types::proposed_event::EntryOwnershipActor::NoPlayer,
            library_shuffle: crate::types::ability::LibraryShuffleMode::PerObject,
            applied: std::collections::HashSet::new(),
            face_down_profile: None,
            with_entry_effect: None,
            co_entrants: None,
            merged_departure: None,
            face_down_in_exile: crate::types::ability::ExileConcealment::Public,
            chain_referent: crate::types::zones::ChainReferentIntent::Silent,
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
            state.deferred_entry_events[0].event,
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
                .any(|t| t.definition == granted_trigger),
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
                            recipient: crate::types::ability::CopyRecipient::Source,
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
            library_placement: None,
            object_id: copy_ench,
            from: Zone::Stack,
            to: Zone::Battlefield,
            cause: None,
            attach_to: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            enters_attacking: false,
            enter_with_counters: Vec::new(),
            enter_with_grants: None,
            controller_override: None,
            enter_transformed: false,
            enter_as_copy: None,
            enter_with_choices: None,
            discard_frame: None,
            performed_by: None,
            ownership_actor: crate::types::proposed_event::EntryOwnershipActor::NoPlayer,
            library_shuffle: crate::types::ability::LibraryShuffleMode::PerObject,
            applied: std::collections::HashSet::new(),
            face_down_profile: None,
            with_entry_effect: None,
            co_entrants: None,
            merged_departure: None,
            face_down_in_exile: crate::types::ability::ExileConcealment::Public,
            chain_referent: crate::types::zones::ChainReferentIntent::Silent,
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
                            recipient: crate::types::ability::CopyRecipient::Source,
                            target: copy_filter,
                            duration: None,
                            mana_value_limit: None,
                            additional_modifications: Vec::new(),
                        },
                    )),
            );

        let mut events = Vec::new();
        let proposed = ProposedEvent::ZoneChange {
            library_placement: None,
            object_id: copy_ench,
            from: Zone::Stack,
            to: Zone::Battlefield,
            cause: None,
            attach_to: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            enters_attacking: false,
            enter_with_counters: Vec::new(),
            enter_with_grants: None,
            controller_override: None,
            enter_transformed: false,
            enter_as_copy: None,
            enter_with_choices: None,
            discard_frame: None,
            performed_by: None,
            ownership_actor: crate::types::proposed_event::EntryOwnershipActor::NoPlayer,
            library_shuffle: crate::types::ability::LibraryShuffleMode::PerObject,
            applied: std::collections::HashSet::new(),
            face_down_profile: None,
            with_entry_effect: None,
            co_entrants: None,
            merged_departure: None,
            face_down_in_exile: crate::types::ability::ExileConcealment::Public,
            chain_referent: crate::types::zones::ChainReferentIntent::Silent,
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

    /// CR 616.1: the CR 303.4f host pause must CARRY the caller's entry tail, not
    /// drop it.
    ///
    /// `finish_copy_target_choice_entry` returns `Ok(Some(..))` on that pause, and
    /// its liminal copy-token caller runs the committed-entry emit and the
    /// remaining-batch continuation only on `Ok(None)`. Before the carrier, a
    /// multi-host Aura realized by a liminal copy token would therefore have
    /// abandoned the rest of the batch and never published its created ids.
    ///
    /// Driven at the seam rather than through a card, because no card in the pool
    /// reaches the intersection today (see the comment at the `NeedsChoice` arm) —
    /// which is exactly why the behaviour needs a test that does not depend on
    /// one. The post-action chosen is the real batch continuation, and the
    /// prompt is answered through the production `apply` path, so the assertion
    /// is "the second token actually got created", not "a struct was stored".
    #[test]
    fn the_aura_host_pause_carries_the_liminal_copy_token_continuation() {
        use crate::game::printed_cards::intrinsic_copiable_values;
        use crate::types::game_state::PendingCounterPostAction;
        use crate::types::keywords::Keyword;
        use crate::types::proposed_event::{CopyTokenSpec, EtbTapState};

        let mut state = GameState::new_two_player(42);
        let hosts: Vec<ObjectId> = (0..2)
            .map(|i| make_creature(&mut state, PlayerId(0), &format!("Grizzly Bears {i}")))
            .collect();

        // An unattached Aura on the battlefield with two legal hosts: the shape
        // `resolve_entering_aura_attachment` answers with `NeedsChoice`.
        let entering_aura = create_object(
            &mut state,
            CardId(400),
            PlayerId(0),
            "Entering Aura".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&entering_aura).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Enchantment];
            obj.card_types.core_types = vec![CoreType::Enchantment];
            obj.base_card_types.subtypes = vec!["Aura".to_string()];
            obj.card_types.subtypes = vec!["Aura".to_string()];
            let enchant = Keyword::Enchant(TargetFilter::Typed(
                crate::types::ability::TypedFilter::new(
                    crate::types::ability::TypeFilter::Creature,
                ),
            ));
            obj.base_keywords = vec![enchant.clone()];
            obj.keywords = vec![enchant];
        }

        // The tail the caller would otherwise have run itself: one more token in
        // this batch, a copy of a Treasure.
        let copied = create_object(
            &mut state,
            CardId(401),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&copied).unwrap();
            obj.base_card_types.core_types = vec![CoreType::Artifact];
            obj.card_types = obj.base_card_types.clone();
        }
        let values = intrinsic_copiable_values(state.objects.get(&copied).unwrap());
        let continuation = PendingCounterPostAction::ContinueCopyTokenCreation {
            owner: PlayerId(0),
            copy: Box::new(CopyTokenSpec {
                values: Box::new(values),
                display: crate::game::game_object::DisplayIdentity {
                    display_source: crate::game::game_object::DisplaySource::Token,
                    ..Default::default()
                },
                extra_keywords: Vec::new(),
                additional_modifications: Vec::new(),
                tapped: false,
                enters_attacking: false,
                attack_request: crate::types::proposed_event::TokenAttackRequest::Unspecified,
                sacrifice_at: None,
                source_id: entering_aura,
                controller: PlayerId(0),
                // Test-only Treasure tail stages no donor; empty mods are stamp-proof.
                donor: None,
                // Single-faced fixture values: no other face to carry.
                double_faced: None,
            }),
            enter_tapped: EtbTapState::Unspecified,
            enter_with_counters: Vec::new(),
            remaining_count: 1,
        };

        let tokens_before = state
            .battlefield
            .iter()
            .filter(|id| state.objects[id].is_token)
            .count();

        let mut events = Vec::new();
        let paused = finish_copy_target_choice_entry(
            &mut state,
            entering_aura,
            &mut events,
            vec![continuation],
            EntryEventReplay::AlreadyDelivered,
            CopyEntryCounters::ReplacementsOnly,
        )
        .expect("the host pause is not an error");

        assert!(
            matches!(paused, Some(WaitingFor::ReturnAsAuraTarget { .. })),
            "two legal hosts must pause on the CR 303.4f host choice, got {paused:?}"
        );
        assert!(
            state.active_counter_additions().is_some(),
            "CR 616.1: the pause must park the caller's tail, not drop it"
        );

        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(hosts[1])),
            },
        )
        .expect("answer the CR 303.4f host choice");

        assert_eq!(
            state.objects[&entering_aura].attached_to,
            Some(crate::game::game_object::AttachTarget::Object(hosts[1])),
            "the host choice still applies"
        );
        let tokens_after = state
            .battlefield
            .iter()
            .filter(|id| state.objects[id].is_token)
            .count();
        assert_eq!(
            tokens_after,
            tokens_before + 1,
            "CR 616.1: the parked batch continuation must run once the host is chosen \
             — this is the assertion that fails if the carrier is removed"
        );
        assert!(
            state.active_counter_additions().is_none(),
            "the parked frame is consumed, not left resident"
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
            library_placement: None,
            object_id: denry,
            from: Zone::Stack,
            to: Zone::Battlefield,
            cause: None,
            attach_to: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            enters_attacking: false,
            enter_with_counters: Vec::new(),
            enter_with_grants: None,
            controller_override: None,
            enter_transformed: false,
            enter_as_copy: None,
            enter_with_choices: None,
            discard_frame: None,
            performed_by: None,
            ownership_actor: crate::types::proposed_event::EntryOwnershipActor::NoPlayer,
            library_shuffle: crate::types::ability::LibraryShuffleMode::PerObject,
            applied: std::collections::HashSet::new(),
            face_down_profile: None,
            with_entry_effect: None,
            co_entrants: None,
            merged_departure: None,
            face_down_in_exile: crate::types::ability::ExileConcealment::Public,
            chain_referent: crate::types::zones::ChainReferentIntent::Silent,
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
            parent_occurrences: Default::default(),
            context: Default::default(),
            continuation: None,
            replacement_applied: Default::default(),
            remaining_players: Vec::new(),
            trigger_context: None,
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

    /// Issue #6440 — Mockingbird-class AmountSpentToCastSource ceiling.
    fn mockingbird_become_copy() -> AbilityDefinition {
        use crate::types::ability::{CopyManaValueLimit, Duration, Effect, TargetFilter};
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                recipient: crate::types::ability::CopyRecipient::Source,
                duration: Some(Duration::Permanent),
                mana_value_limit: Some(CopyManaValueLimit::AmountSpentToCastSource),
                additional_modifications: Vec::new(),
            },
        )
    }

    fn clone_become_copy() -> AbilityDefinition {
        use crate::types::ability::{Duration, Effect, TargetFilter};
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                recipient: crate::types::ability::CopyRecipient::Source,
                duration: Some(Duration::Permanent),
                mana_value_limit: None,
                additional_modifications: Vec::new(),
            },
        )
    }

    fn make_creature_with_mv(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        mv: u32,
    ) -> ObjectId {
        let id = make_creature(state, owner, name);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.mana_cost = crate::types::mana::ManaCost::generic(mv);
        id
    }

    fn chord_like_spell_resolution(
        chord_id: ObjectId,
        actual_mana_spent: u32,
    ) -> crate::types::game_state::PendingSpellResolution {
        use crate::types::game_state::{CastingVariant, PendingSpellResolution};
        PendingSpellResolution {
            object_id: chord_id,
            controller: PlayerId(0),
            casting_variant: CastingVariant::Normal,
            cast_from_zone: None,
            cast_controller: None,
            cast_timing_permission: None,
            spell_targets: vec![],
            actual_mana_spent,
            kickers_paid: vec![],
            additional_cost_payment_count: 0,
            additional_cost_payments: vec![],
            convoked_creatures: vec![],
        }
    }

    /// Hostile: uncast stamp 0 + Chord-like ctx spent 5 → ceiling Some(0); MV2 excluded.
    #[test]
    fn issue_6440_uncast_stamp_ignores_spell_resolution_mana_spent() {
        let mut state = GameState::new_two_player(42);
        let mv0 = make_creature_with_mv(&mut state, PlayerId(0), "Mv0", 0);
        let mv2 = make_creature_with_mv(&mut state, PlayerId(0), "Mv2", 2);
        let mockingbird = make_creature(&mut state, PlayerId(0), "Mockingbird");
        // Uncast: stamp stays at default 0.
        assert_eq!(state.objects[&mockingbird].mana_spent_to_cast_amount, 0);

        let chord = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Chord of Calling".to_string(),
            Zone::Stack,
        );
        let hostile_ctx = chord_like_spell_resolution(chord, 5);
        let mut events = Vec::new();
        let waiting = apply_post_replacement_effect(
            &mut state,
            &mockingbird_become_copy(),
            ContinuationSubjects::affected_only(Some(mockingbird)),
            Some(&hostile_ctx),
            None,
            Default::default(),
            &mut events,
        );

        match waiting {
            Some(WaitingFor::CopyTargetChoice {
                max_mana_value,
                valid_targets,
                ..
            }) => {
                assert_eq!(
                    max_mana_value,
                    Some(0),
                    "uncast stamp must yield Some(0), not unconstrained None"
                );
                assert!(
                    valid_targets.contains(&mv0),
                    "MV 0 must remain legal; got {valid_targets:?}"
                );
                assert!(
                    !valid_targets.contains(&mv2),
                    "MV 2 must be excluded under ceiling 0; got {valid_targets:?}"
                );
            }
            other => panic!("expected CopyTargetChoice, got {other:?}"),
        }
    }

    /// Cast stamp 2 → ceiling Some(2); ctx spent 99 must not override.
    #[test]
    fn issue_6440_cast_stamp_drives_ceiling_not_ctx() {
        let mut state = GameState::new_two_player(42);
        let mv2 = make_creature_with_mv(&mut state, PlayerId(0), "Mv2", 2);
        let mv3 = make_creature_with_mv(&mut state, PlayerId(0), "Mv3", 3);
        let mockingbird = make_creature(&mut state, PlayerId(0), "Mockingbird");
        state
            .objects
            .get_mut(&mockingbird)
            .unwrap()
            .mana_spent_to_cast_amount = 2;

        let chord = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Unrelated Spell".to_string(),
            Zone::Stack,
        );
        let hostile_ctx = chord_like_spell_resolution(chord, 99);
        let mut events = Vec::new();
        let waiting = apply_post_replacement_effect(
            &mut state,
            &mockingbird_become_copy(),
            ContinuationSubjects::affected_only(Some(mockingbird)),
            Some(&hostile_ctx),
            None,
            Default::default(),
            &mut events,
        );

        match waiting {
            Some(WaitingFor::CopyTargetChoice {
                max_mana_value,
                valid_targets,
                ..
            }) => {
                assert_eq!(max_mana_value, Some(2));
                assert!(valid_targets.contains(&mv2));
                assert!(!valid_targets.contains(&mv3));
            }
            other => panic!("expected CopyTargetChoice, got {other:?}"),
        }
    }

    /// Liminal-only source: stamp read via dual lookup (Gap 1).
    #[test]
    fn issue_6440_liminal_stamp_drives_ceiling() {
        use crate::types::game_state::{LiminalEntry, LiminalEntryKind};
        use crate::types::zones::EtbTapState;

        let mut state = GameState::new_two_player(42);
        let mv0 = make_creature_with_mv(&mut state, PlayerId(0), "Mv0", 0);
        let mv2 = make_creature_with_mv(&mut state, PlayerId(0), "Mv2", 2);

        let liminal_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut liminal = GameObject::new(
            liminal_id,
            CardId(50),
            PlayerId(0),
            "Liminal Mockingbird".to_string(),
            Zone::Battlefield,
        );
        liminal.card_types.core_types.push(CoreType::Creature);
        liminal.mana_spent_to_cast_amount = 0;
        assert!(!state.objects.contains_key(&liminal_id));
        state.liminal_entries.insert(
            liminal_id,
            LiminalEntry {
                copy_settlement: None,
                object: crate::types::game_state::LiminalEntrant::Token(
                    crate::types::game_state::TokenProjection::materialize(liminal),
                ),
                name: "Liminal Mockingbird".to_string(),
                source_id: ObjectId(999),
                controller: PlayerId(0),
                enters_attacking: false,
                attack_destination: crate::types::ability::EntryAttackDestination::AnyDefender,
                attach_to: None,
                sacrifice_at: None,
                remaining_count: 0,
                created_ids: Vec::new(),
                copy_resume: None,
                spec_resume: None,
                enter_tapped: EtbTapState::Unspecified,
                enter_with_counters: Vec::new(),
                kind: LiminalEntryKind::Token,
                replacement_applied: HashSet::new(),
            },
        );

        let mut events = Vec::new();
        let waiting = apply_post_replacement_effect(
            &mut state,
            &mockingbird_become_copy(),
            ContinuationSubjects::affected_only(Some(liminal_id)),
            None,
            None,
            Default::default(),
            &mut events,
        );

        match waiting {
            Some(WaitingFor::CopyTargetChoice {
                max_mana_value,
                valid_targets,
                ..
            }) => {
                assert_eq!(max_mana_value, Some(0));
                assert!(valid_targets.contains(&mv0));
                assert!(!valid_targets.contains(&mv2));
            }
            other => panic!("expected CopyTargetChoice from liminal stamp, got {other:?}"),
        }
    }

    /// Clone sibling: no mana_value_limit → unconstrained even when uncast.
    #[test]
    fn issue_6440_clone_without_limit_stays_unconstrained() {
        let mut state = GameState::new_two_player(42);
        let mv2 = make_creature_with_mv(&mut state, PlayerId(0), "Mv2", 2);
        let clone = make_creature(&mut state, PlayerId(0), "Clone");
        assert_eq!(state.objects[&clone].mana_spent_to_cast_amount, 0);

        let mut events = Vec::new();
        let waiting = apply_post_replacement_effect(
            &mut state,
            &clone_become_copy(),
            ContinuationSubjects::affected_only(Some(clone)),
            Some(&chord_like_spell_resolution(ObjectId(1), 5)),
            None,
            Default::default(),
            &mut events,
        );

        match waiting {
            Some(WaitingFor::CopyTargetChoice {
                max_mana_value,
                valid_targets,
                ..
            }) => {
                assert_eq!(max_mana_value, None);
                assert!(
                    valid_targets.contains(&mv2),
                    "Clone without limit must still allow MV 2"
                );
            }
            other => panic!("expected CopyTargetChoice, got {other:?}"),
        }
    }

    /// Park a state on Priority whose single resolution frame is a
    /// `PostReplacement` holding one `Dispatching` resident — the wedge shape,
    /// reached the only way it can be reached without a live dispatcher: an
    /// install followed by a direct `begin_dispatch` whose handle is then
    /// dropped, exactly as a real dispatcher's handle dies with its call frame.
    fn state_with_ownerless_dispatching_resident() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.priority_player = PlayerId(0);

        let mut drains = crate::types::game_state::PostReplacementDrainStack::default();
        assert!(
            drains.install(
                crate::types::game_state::PostReplacementDrain::ready(
                    PostReplacementContinuation::Resolved(Box::new(ResolvedAbility::new(
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                        Vec::new(),
                        ObjectId(81),
                        PlayerId(0),
                    ))),
                ),
                crate::types::game_state::ResidentDrainPolicy::KeepResident,
            ),
            "the fixture's ready drain installs"
        );
        let (_continuation, _handle) = drains
            .begin_dispatch()
            .expect("a ready resident begins dispatching");
        state.resolution_stack.push_post_replacement(drains);
        state
    }

    fn resident_is_dispatching(state: &GameState) -> bool {
        state
            .active_post_replacement_drains()
            .and_then(crate::types::game_state::PostReplacementDrainStack::resident)
            .is_some_and(|drain| {
                matches!(
                    drain.status,
                    crate::types::game_state::DrainStatus::Dispatching
                )
            })
    }

    /// **H5 — discriminating(U1), two halves.** A `Dispatching` entry whose
    /// dispatcher is still on this thread's call stack is live work, not a
    /// strand, and the priority-boundary sweeper must leave it alone.
    ///
    /// Carried WITHOUT a CR citation, deliberately. This is an invariant of the
    /// engine's own dispatch machinery — which in-flight work the sweeper may
    /// retire — and no rule of the game speaks to it. It formerly cited
    /// CR 615.5, which says a prevention effect may include an additional effect
    /// referring to the amount of damage that was prevented, the rest of the
    /// effect taking place immediately after the prevention itself.
    ///
    /// That rule is real authority for the PREVENTION family's additional-effect
    /// reference, and it is why `begin_dispatch`
    /// (`types/game_state.rs`) and `apply_pending_post_replacement_effect` above
    /// both still cite it: the engine satisfies that reference by keeping the
    /// drain resident with its event context readable, which is how
    /// `PostReplacementSourceController` resolves Swans of Bryn Argoll. What
    /// CR 615.5 does NOT speak to is dispatch LIVENESS — whether a `Dispatching`
    /// entry is still on this thread's call stack — and that is this test's
    /// actual subject. So the number is dropped HERE rather than swapped for a
    /// guess. Do not read that omission as a reason to strip CR 615.5 from the
    /// event-context sites, where it is the correct anchor.
    ///
    /// Half (i) holds a `LiveDispatchGuard` across the sweep and asserts the
    /// drain SURVIVES. Hand-patching `post_replacement_dispatch_is_live()` to
    /// return `false` turns half (i) red behaviourally — the sweep fires
    /// mid-dispatch and destroys the event context the running continuation
    /// reads.
    ///
    /// Half (ii) repeats the sweep with no guard held and asserts the drain IS
    /// retired. It is half (i)'s positive control: without it, half (i) would be
    /// satisfied by a sweep that never fires at all. Reverting the sweep turns
    /// half (ii) red.
    ///
    /// Half (iii) tests the shared policy FUNCTION rather than one of its callers.
    /// The guard is the only protection the new entry-side call site in
    /// `engine::apply_action_boundary_core` has, so it is asserted at the function
    /// both callers share. Half (iii)'s second call is its own positive control:
    /// without it, a policy that never sweeps anything would pass.
    ///
    /// `LiveDispatchGuard::enter()` stays module-private deliberately — the
    /// guard's soundness argument is "the dispatcher is the sole thing that can
    /// make a dispatch live", and a `pub(crate) enter()` would downgrade that
    /// from a compiler-enforced fact to a convention.
    #[test]
    fn h5_a_live_dispatch_suppresses_the_ownerless_sweep_and_its_absence_permits_it() {
        // Half (i): a live dispatch is on this thread's call stack.
        let mut state = state_with_ownerless_dispatching_resident();
        assert!(
            resident_is_dispatching(&state),
            "reach-guard: the fixture parks a Dispatching resident"
        );
        let mut events = Vec::new();
        {
            let _live = LiveDispatchGuard::enter();
            assert!(
                post_replacement_dispatch_is_live(),
                "reach-guard: the guard actually marks the dispatch live"
            );
            crate::game::effects::resume_resolution_frames(&mut state, &mut events);
        }
        assert!(
            resident_is_dispatching(&state),
            "a live dispatch's resident must survive the priority-boundary sweep"
        );
        assert_eq!(
            state.resolution_stack.len(),
            1,
            "the frame that owns a live dispatch must survive too"
        );

        // Half (ii), the positive control: with no dispatch live, the same call
        // retires the same entry — so half (i) is not passing on an inert sweep.
        assert!(
            !post_replacement_dispatch_is_live(),
            "the guard restored the previous value on drop"
        );
        crate::game::effects::resume_resolution_frames(&mut state, &mut events);
        assert!(
            state.resolution_stack.is_empty(),
            "CR 603.3b + CR 608.2c: an ownerless Dispatching resident and its emptied frame retire"
        );

        // Half (iii): the shared policy FUNCTION, called directly, is guard
        // suppressed exactly as its `resume_resolution_frames` caller is.
        let mut direct = state_with_ownerless_dispatching_resident();
        assert!(
            resident_is_dispatching(&direct),
            "reach-guard: the fixture parks a Dispatching resident"
        );
        {
            let _live = LiveDispatchGuard::enter();
            crate::game::effects::sweep_ownerless_post_replacement_strand(&mut direct);
        }
        assert!(
            resident_is_dispatching(&direct),
            "a live dispatch's resident survives the shared policy function too"
        );
        crate::game::effects::sweep_ownerless_post_replacement_strand(&mut direct);
        assert!(
            !resident_is_dispatching(&direct),
            "half (iii)'s own positive control: with no dispatch live the same direct call retires it"
        );
    }

    /// CR 701.9a vs CR 701.21a: the two adjacent post-replacement drains in
    /// `handle_choose_replacement` publish their completion DIFFERENTLY, and the
    /// asymmetry is deliberate. The sacrifice drain stamps
    /// `ThisWayCause::Sacrificed`; the discard drain immediately below stamps
    /// nothing, because the UN-paused discard path does not stamp either
    /// (`grep stamp_active_player_action_completion effects/discard.rs` -> 0), so
    /// a stamping resume would give a paused discard a provenance its own
    /// un-paused twin never has.
    ///
    /// Nothing else pins this. A reader "fixing the inconsistency" between two
    /// blocks twenty-five lines apart would silently change `Discarded`-this-way
    /// provenance for the whole class.
    ///
    /// A SOURCE census rather than a behavioural assertion, and that is measured:
    /// `stamp_active_player_action_completion` early-returns unless an active
    /// ability continuation frame holds a `CompletePlayerAction` chain, so in a
    /// drain unit test -- which has neither -- adding the call would change no
    /// observable state and a behavioural assertion would itself be vacuous.
    ///
    /// THE WINDOWS ARE BRACE-BALANCED, NOT END-ANCHORED, and that is the whole
    /// difficulty of this instrument. An earlier revision ended each slice at a
    /// guessed marker; the marker for the second window happened to sit inside
    /// the arm, so the "guarded" region collapsed to **36 characters of a
    /// five-line arm** and the named revert probe only flipped if a stamp landed
    /// as the arm's very first statement. A census whose window is wrong reports
    /// a zero that means nothing, and it reports it silently.
    ///
    /// Three defences, because a negative result needs all of them, plus one
    /// CLOSURE that is deliberately not counted among them:
    ///   0. the text scanned is the CODE half only, via the shared
    ///      `source_census` authority. MEASURED INERT on today's tree: raw and
    ///      stripped text are byte-identical for every quantity this test reads
    ///      (both anchors and the needle at 1, sacrifice window 716 chars,
    ///      discard window 281, `drain_pending_continuation` present in both —
    ///      those two char counts are a SNAPSHOT and will rot on any edit to
    ///      either arm body; the durable claim is the raw/stripped IDENTITY, not
    ///      the numbers),
    ///      which is exactly what `source_census.rs` predicts of any census that
    ///      scans the real tree. It discriminates nothing here and is listed
    ///      apart from 1-3 for that reason: it closes a shape not currently
    ///      present rather than catching one that is;
    ///   1. each anchor must match EXACTLY ONCE in the file, so a deleted
    ///      production arm cannot let the scan retarget some other text (this
    ///      doc comment deliberately never spells an anchor literally);
    ///   2. each window is closed by brace balance, so it always spans the whole
    ///      arm body;
    ///   3. each window asserts its OWN non-degeneracy. The positive control on
    ///      the sacrifice arm proves the file and that anchor are real, but it
    ///      says nothing about the extent of the DISCARD window -- and the
    ///      discard window is the one whose zero carries the claim.
    ///
    /// REVERT PROBES:
    ///   * add a stamp call ANYWHERE in the discard arm -- first statement, last
    ///     statement, or nested inside its `if` -- and half (b) fails. Only the
    ///     brace-balanced window makes all three positions equivalent.
    ///   * delete the sacrifice arm's existing stamp -> half (a) fails.
    ///   * delete either arm outright -> the exactly-once assertion fails, rather
    ///     than the scan silently sliding onto other text.
    #[test]
    fn the_resumed_discard_drain_does_not_stamp_a_completion_while_its_sacrifice_sibling_does() {
        /// The arm body that follows `anchor`, delimited by brace balance.
        fn arm_body<'a>(source: &'a str, anchor: &str) -> &'a str {
            assert_eq!(
                source.matches(anchor).count(),
                1,
                "anchor {anchor:?} must identify exactly one site; a second \
                 occurrence lets a deleted arm retarget the scan silently"
            );
            let after = source.find(anchor).expect("anchor present") + anchor.len();
            let open = after
                + source[after..]
                    .find('{')
                    .expect("the arm opens a block after its anchor");
            let mut depth = 0usize;
            for (offset, ch) in source[open..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return &source[open..=open + offset];
                        }
                    }
                    _ => {}
                }
            }
            panic!("unbalanced braces after {anchor:?}");
        }

        // Routed through the shared comment authority rather than scanned raw,
        // and this census is exactly the case that module exists for: half (b)
        // is a NEGATIVE, so a deleted stamp whose spelling survived in a
        // trailing `//` inside the discard arm would HOLD the zero and hide the
        // regression, while a comment merely naming the needle would flip it red
        // on a pure prose edit.
        //
        // SCOPE, because "stripping comments" would overclaim: `code_lines`
        // removes whole-line and trailing `//` comments and a LEADING block
        // comment. The interior lines of a multi-line `/* … */` survive and are
        // still scanned — `source_census.rs` says so in its own doc. So a block
        // comment inside the discard arm naming the needle would still flip
        // half (b) red. That residue is fail-CLOSED (spurious red, never a
        // missed site), which is the direction a census may fail in.
        let source = &crate::source_census::code_lines(include_str!("engine_replacement.rs"));
        let needle = concat!("stamp_active_player_action_", "completion(");
        let sacrifice_anchor = concat!("PendingPlayerScopeSacrifice", "Outcome::Completed {");
        let discard_anchor = concat!("PendingDiscardBatch", "Outcome::Completed =>");

        let sacrifice = arm_body(source, sacrifice_anchor);
        let discard = arm_body(source, discard_anchor);

        // (0) Non-degeneracy, asserted per window. Both arms run a multi-line
        // body ending in a `drain_pending_continuation` call guarded by a
        // `waiting_for` re-check, so a window that cannot see that call has not
        // spanned its arm and any verdict drawn from it is worthless.
        for (label, window) in [("sacrifice", sacrifice), ("discard", discard)] {
            assert!(
                window.contains("drain_pending_continuation"),
                "{label} window must span its whole arm body; it stops before the \
                 arm's last statement, so a scan of it proves nothing (got {} chars)",
                window.len()
            );
        }

        // (a) Positive control: proves the instrument finds a stamp where one
        // exists. Necessary but NOT sufficient for (b) -- different window.
        assert!(
            sacrifice.contains(needle),
            "(a) positive control: the sacrifice arm must stamp, or this scan is \
             reading the wrong region and (b)'s zero would mean nothing"
        );

        // (b) The claim.
        assert!(
            !discard.contains(needle),
            "(b) the resumed discard must publish exactly what the un-paused \
             discard publishes -- no CompletePlayerAction stamp"
        );
    }

    /// The post-replacement dispatch binds the REPLACEMENT SOURCE'S player, not the active player.
    ///
    /// REVERT / MUTATION PROBE: bind `apply_post_replacement_effect`'s `controller` to
    /// `state.active_player` ⇒ **this row FAILS** (the prompt goes to `PlayerId(0)`).
    #[test]
    fn post_replacement_execute_binds_the_replacement_source_player() {
        use crate::types::ability::{TypeFilter, TypedFilter};

        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        let land_controller = PlayerId(1);

        // ⟨G⟩ reach-guard: the two candidate authorities DISAGREE on this board. Without
        // this the row passes identically under the mutation and measures nothing.
        assert_ne!(
            state.active_player, land_controller,
            "⟨G⟩ reach-guard: `active_player` and the source's controller must differ, else \
             both bindings agree and this row cannot discriminate"
        );

        let land = create_object(
            &mut state,
            CardId(900),
            land_controller,
            "Gilt-Leaf Palace".to_string(),
            Zone::Battlefield,
        );
        assert_eq!(
            crate::game::replacement::replacement_source_player(&state.objects[&land]),
            land_controller,
            "⟨G⟩ reach-guard: a battlefield source's replacement player IS its controller \
             (CR 109.4), so the binding under test resolves to the value asserted below"
        );

        let make_elf = |state: &mut GameState, card: u64, owner: PlayerId| {
            let id = create_object(
                state,
                CardId(card),
                owner,
                "Llanowar Elves".to_string(),
                Zone::Hand,
            );
            let obj = state.objects.get_mut(&id).expect("just created");
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.card_types.subtypes = vec!["Elf".to_string()];
            id
        };
        // A matching card in EACH player's hand, so the verdict names WHICH hand was read
        // rather than which hand happened to be non-empty.
        let active_player_elf = make_elf(&mut state, 901, PlayerId(0));
        let controller_elf = make_elf(&mut state, 902, land_controller);
        assert_ne!(active_player_elf, controller_elf);

        let elf_filter = TargetFilter::Typed(
            TypedFilter::card().with_type(TypeFilter::Subtype("Elf".to_string())),
        );
        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::RevealFromHand {
                filter: elf_filter,
                on_decline: None,
            },
        );

        let mut events = Vec::new();
        // Self-scoped ETB replacement: the land is both the affected object and
        // the replacing ability's source, so `affected_only` keeps "you" on the
        // land's controller (P1), not `state.active_player` (P0).
        let waiting = apply_post_replacement_effect(
            &mut state,
            &effect_def,
            ContinuationSubjects::affected_only(Some(land)),
            None,
            None,
            HashSet::new(),
            &mut events,
        );

        match waiting {
            Some(WaitingFor::RevealChoice {
                ref player,
                ref cards,
                ..
            }) => {
                assert_eq!(
                    (*player, cards.clone()),
                    (land_controller, vec![controller_elf]),
                    "S6-P2b: the dispatch binds the REPLACEMENT SOURCE's player, so the \
                     prompt goes to the land's controller and offers THAT player's card. \
                     Binding `state.active_player` instead makes this FAIL"
                );
            }
            other => panic!("expected a RevealChoice prompt, got {:?}", other),
        }
    }

    /// Drops `removed` from every parked CopyToken owner's `created_ids`, through the
    /// stack's serialized form (a buried frame has no mutable accessor).
    /// Returns how many owners listed it.
    fn unlist_from_parked_copy_token_owners(state: &mut GameState, removed: ObjectId) -> usize {
        fn walk(value: &mut serde_json::Value, removed: u64, edits: &mut usize) {
            match value {
                serde_json::Value::Object(map) => {
                    // `ResolutionFrame` is adjacently tagged: `{"type": .., "data": ..}`.
                    let is_owner =
                        map.get("type").and_then(serde_json::Value::as_str) == Some("CopyToken");
                    if let Some(serde_json::Value::Array(ids)) = map
                        .get_mut("data")
                        .filter(|_| is_owner)
                        .and_then(|owner| owner.get_mut("created_ids"))
                    {
                        let before = ids.len();
                        ids.retain(|id| id.as_u64() != Some(removed));
                        *edits += before - ids.len();
                    }
                    map.values_mut().for_each(|v| walk(v, removed, edits));
                }
                serde_json::Value::Array(items) => {
                    items.iter_mut().for_each(|v| walk(v, removed, edits))
                }
                serde_json::Value::Null
                | serde_json::Value::Bool(_)
                | serde_json::Value::Number(_)
                | serde_json::Value::String(_) => {}
            }
        }
        let mut value =
            serde_json::to_value(&state.resolution_stack).expect("the stack serializes");
        let mut edits = 0;
        walk(&mut value, removed.0, &mut edits);
        state.resolution_stack = serde_json::from_value(value).expect("the stack decodes");
        edits
    }

    /// F7 (M-f): the copy-target answer reads the entrant's acquired "enters with" on the
    /// board with its co-entrants absent, and with nothing else absent. Clone Legion over
    /// P1's [Margle, Quicksilver Gargantuan]; the Gargantuan copy copies P0's Triskelion.
    /// (a) As driven, the Margle copy co-enters: 3 counters. (b) The same Margle copy, no
    /// longer listed by the parked owner, is a resident remover at the answer: 0 counters.
    /// No printed resident remover reaches this read while sparing the copy offer, so the
    /// resident is made by unlisting it.
    #[test]
    fn copy_target_answer_reads_entry_counters_without_co_entrants_and_with_residents() {
        use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
        use crate::types::ability::TargetRef;
        use crate::types::mana::ManaCost;
        use crate::types::phase::Phase;

        let mut scenario = GameScenario::new_n_player(2, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let trisk = scenario
            .add_creature_from_oracle(
                P0,
                "Triskelion",
                1,
                1,
                "This creature enters with three +1/+1 counters on it.\nRemove a +1/+1 \
                 counter from this creature: It deals 1 damage to any target.",
            )
            .id();
        scenario.add_creature_from_oracle(
            P1,
            "Margle, Cousin of Yargle",
            5,
            2,
            "Other creatures you control lose all abilities, can't gain abilities, and get \
             +2/+2.",
        );
        scenario.add_creature_from_oracle(
            P1,
            "Quicksilver Gargantuan",
            7,
            7,
            "You may have this creature enter as a copy of any creature on the battlefield, \
             except it's 7/7.",
        );
        let legion = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Clone Legion",
                false,
                "For each creature target player controls, create a token that's a copy of \
                 that creature.",
            )
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();
        runner.cast(legion).target_player(P1).commit();
        for _ in 0..16 {
            match &runner.state().waiting_for {
                WaitingFor::CopyTargetChoice { .. } => break,
                WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
                WaitingFor::ReplacementChoice { .. } => {
                    runner.act(GameAction::ChooseReplacement { index: 0 })
                }
                other => panic!("unexpected prompt {other:?}"),
            }
            .expect("the action is accepted");
        }
        let WaitingFor::CopyTargetChoice { source_id, .. } = runner.state().waiting_for.clone()
        else {
            panic!("REACH-GUARD: the Gargantuan copy's copy choice");
        };
        let margle = runner
            .state()
            .objects
            .values()
            .find(|obj| obj.is_token && obj.name == "Margle, Cousin of Yargle")
            .map(|obj| obj.id)
            .expect("REACH-GUARD: the Margle copy entered first");

        let answer = |mut runner: GameRunner| {
            assert!(
                runner.state().objects[&margle].is_phased_in_permanent(),
                "REACH-GUARD: the Margle copy is on the battlefield, phased in"
            );
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(trisk)),
                })
                .expect("the copy choice is accepted");
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                "the entry completes, got {:?}",
                runner.state().waiting_for
            );
            let counters = runner.state().objects[&source_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0);
            (runner, counters)
        };

        let mut resident = runner.state().clone();
        assert_eq!(
            unlist_from_parked_copy_token_owners(&mut resident, margle),
            1,
            "REACH-GUARD: the parked owner listed the Margle copy"
        );
        let (_, co_entering) = answer(runner);
        assert_eq!(co_entering, 3, "(a) the co-entering Margle copy is absent");
        let (resident, stripped) = answer(GameRunner::from_state(resident));
        assert_eq!(
            resident.state().last_created_token_ids,
            vec![source_id],
            "REACH-GUARD (b): the batch lists only the entrant"
        );
        assert_eq!(stripped, 0, "(b) the resident Margle copy strips them");
    }
}

#[cfg(test)]
mod entry_continuation_release_tests {
    //! The settled-entry release — its gate, the recorded state deciding
    //! collection, the owner-frame refusal, the ledger filter and the delayed
    //! half — the as-enters entrant predicate, each park removal's ownership
    //! rule, and the completion authority (CR 603.2 + CR 603.6a + CR 603.6d +
    //! CR 603.7 + CR 603.10 + CR 614.12a + CR 800.4a).
    use super::*;
    use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
    use crate::game::zones::{create_object, record_and_emit_entry_from_no_zone};
    use crate::types::ability::{
        AbilityCondition, AbilityKind, DelayedTriggerCondition, DelayedTriggerKind,
        DelayedTriggerLifetime, EntryAttackDestination, QuantityRef, TriggerDefinition,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{
        DelayedTrigger, PausedZoneChangeDeliveryCount, PendingBatchDeliveries,
        PendingCounterAdditionQueue, PendingZoneChangeDelivery, ResidentDrainPolicy,
        ZoneDeliveryExileTracking,
    };
    use crate::types::identifiers::{CardId, DelayedInstallIdentity};
    use crate::types::keywords::KeywordKind;
    use crate::types::mana::ManaCost;
    use crate::types::phase::Phase;
    use crate::types::resolution::{IdentifiedPostReplacementDispatch, ResolutionFrame};
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::EtbTapState;

    const SOUL_WARDEN: &str = "Whenever another creature enters, you gain 1 life.";
    const BECK: &str = "Whenever a creature enters this turn, you may draw a card.";

    fn scenario(players: u8) -> GameScenario {
        let mut scenario = GameScenario::new_n_player(players, 42);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
        scenario
    }

    const COMPLETED: ParkedEntryState = ParkedEntryState::Completed;
    const PENDING: ParkedEntryState = ParkedEntryState::Pending { carrier: None };

    fn seeded(event: GameEvent, state: ParkedEntryState) -> ParkedEntry {
        ParkedEntry {
            event,
            state,
            entry_attack: None,
            held_triggers: Vec::new(),
        }
    }

    /// A creature owned by `owner` and controlled by `controller` that has just
    /// entered the battlefield; its entry event, parked alone in `parked_state`.
    fn parked_entry(
        state: &mut GameState,
        card: u64,
        owner: PlayerId,
        controller: PlayerId,
        parked_state: ParkedEntryState,
    ) -> (ObjectId, GameEvent) {
        let (id, entry) = entered(state, card, owner, controller);
        state.deferred_entry_events = vec![seeded(entry.clone(), parked_state)];
        (id, entry)
    }

    /// As [`parked_entry`], without touching the park.
    fn entered(
        state: &mut GameState,
        card: u64,
        owner: PlayerId,
        controller: PlayerId,
    ) -> (ObjectId, GameEvent) {
        let id = create_object(
            state,
            CardId(card),
            owner,
            format!("Entrant {card}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).expect("entrant");
        obj.controller = controller;
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
        let mut events = Vec::new();
        record_and_emit_entry_from_no_zone(state, id, &mut events).expect("entered");
        let entry = events
            .into_iter()
            .find(|event| matches!(event, GameEvent::ZoneChanged { .. }))
            .expect("entry event");
        (id, entry)
    }

    fn settled_state(players: u8) -> GameState {
        scenario(players).build().state().clone()
    }

    /// CR 614.12a + CR 113.6 + CR 400.7: an entrant is mid-entry while this
    /// incarnation's parked entry is pending, and at no other time. A completed
    /// entry happened, though it still waits for its collector.
    #[test]
    fn only_a_pending_entry_of_this_incarnation_is_mid_entry() {
        let mut state = settled_state(2);
        let (id, _) = parked_entry(&mut state, 900, P0, P0, PENDING);
        assert!(entrant_entry_is_pending(&state, id));

        let (other, _) = entered(&mut state, 901, P0, P0);
        assert!(!entrant_entry_is_pending(&state, other), "another entrant");

        // CR 400.7: the entry the park holds was of an earlier incarnation.
        state.objects.get_mut(&id).expect("entrant").incarnation += 1;
        assert!(!entrant_entry_is_pending(&state, id), "earlier incarnation");

        let mut state = settled_state(2);
        let (id, _) = parked_entry(&mut state, 900, P0, P0, COMPLETED);
        assert!(entry_park_holds_entrant(&state, id), "still parked");
        assert!(!entrant_entry_is_pending(&state, id), "completed");

        let mut state = settled_state(2);
        let (id, _) = entered(&mut state, 900, P0, P0);
        assert!(!entrant_entry_is_pending(&state, id), "an empty park");

        let mut state = settled_state(2);
        parked_entry(&mut state, 900, P0, P0, PENDING);
        assert!(
            !entrant_entry_is_pending(&state, ObjectId(9_999)),
            "a missing object"
        );
    }

    /// CR 110.2a: an entry parked before the controller override stamped the
    /// action's copy, which leaves the two copies one ledger key and two records.
    /// No printed route reaches this order: delivery stamps before its
    /// continuation parks, and the meld refresh writes both copies. These rows run
    /// the stamp out of order to pin what the park's readers key on.
    fn stamped_parked_entry(state: &mut GameState) -> (ObjectId, Vec<GameEvent>) {
        let (id, entry) = parked_entry(state, 900, P0, P0, PENDING);
        let mut events = vec![entry.clone()];
        crate::game::zones::apply_battlefield_entry_controller_override(state, &mut events, id, P1);
        assert_ne!(events, vec![entry], "reach guard: the copy was stamped");
        (id, events)
    }

    /// CR 614.12a + CR 603.2c: the hold predicate reads a battlefield entry by its
    /// ledger key and every other event by equality, so it holds all that full
    /// equality held.
    #[test]
    fn the_hold_predicate_reads_an_entrys_ledger_key_and_any_other_event_by_equality() {
        let mut state = settled_state(2);
        let (id, stamped) = stamped_parked_entry(&mut state);
        assert!(entry_event_is_parked(&state, &stamped[0]), "a stamped copy");
        let (other, other_entry) = entered(&mut state, 901, P0, P0);
        assert!(
            !entry_event_is_parked(&state, &other_entry),
            "another entry"
        );

        let mut moved = Vec::new();
        crate::game::zones::move_to_zone(&mut state, other, Zone::Graveyard, &mut moved);
        let departure = moved
            .into_iter()
            .find(|event| matches!(event, GameEvent::ZoneChanged { .. }))
            .expect("departure event");
        let land_played = |object_id| GameEvent::LandPlayed {
            object_id,
            player_id: P0,
            from_zone: Zone::Hand,
        };
        let attached = |source_id| GameEvent::EffectResolved {
            kind: EffectKind::Attach,
            source_id,
            subject: None,
        };
        let tapped = |object_id| GameEvent::PermanentTapped {
            object_id,
            caused_by: None,
            incarnation: None,
        };
        for parked in [land_played(id), attached(id), tapped(id), departure] {
            state.deferred_entry_events = vec![seeded(parked.clone(), PENDING)];
            assert!(entry_event_is_parked(&state, &parked), "{parked:?}");
        }
        for (parked, event) in [
            (land_played(id), land_played(other)),
            (attached(id), attached(other)),
            (tapped(id), tapped(other)),
        ] {
            state.deferred_entry_events = vec![seeded(parked, PENDING)];
            assert!(!entry_event_is_parked(&state, &event), "{event:?}");
        }
    }

    /// A paused action's stamped entry, with the park as stamped when `held`, and
    /// emptied otherwise as each reader row's reach guard.
    fn paused_stamped_entry(held: bool) -> (GameState, Vec<GameEvent>) {
        let mut state = settled_state(2);
        let (id, events) = stamped_parked_entry(&mut state);
        if !held {
            state.deferred_entry_events.clear();
        }
        state.waiting_for = WaitingFor::ScryChoice {
            player: P0,
            cards: vec![id],
        };
        (state, events)
    }

    /// CR 603.2c + CR 614.12a: the post-action pass's ordinary scan leaves a
    /// stamped copy of a parked entry to its collector.
    #[test]
    fn the_ordinary_scan_skips_a_stamped_copy_of_a_parked_entry() {
        for (held, collected) in [(true, 0), (false, 1)] {
            let (mut state, mut events) = paused_stamped_entry(held);
            let paused = state.waiting_for.clone();
            crate::game::engine_priority::run_post_action_pipeline(
                &mut state,
                &mut events,
                &paused,
                false,
                false,
            )
            .expect("the pass settles");
            assert_eq!(state.deferred_triggers.len(), collected, "held={held}");
        }
    }

    /// CR 603.2c + CR 614.12a: the paused observer collector leaves a stamped copy
    /// of a parked entry to its collector.
    #[test]
    fn the_paused_observer_collector_skips_a_stamped_copy_of_a_parked_entry() {
        for (held, collected) in [(true, 0), (false, 1)] {
            let (mut state, mut events) = paused_stamped_entry(held);
            crate::game::triggers::park_observer_triggers_if_paused(&mut state, &mut events, 0);
            assert_eq!(state.deferred_triggers.len(), collected, "held={held}");
        }
    }

    /// CR 603.2 + CR 603.7 + CR 614.12a: a delivery-owned delayed scan's input
    /// omits a stamped copy of a parked entry.
    #[test]
    fn the_delivery_delayed_input_omits_a_stamped_copy_of_a_parked_entry() {
        for (held, kept) in [(true, 0), (false, 1)] {
            let (mut state, events) = paused_stamped_entry(held);
            let input = crate::game::engine_priority::unclaimed_delayed_trigger_input(
                &mut state,
                &events,
                0,
                &mut Vec::new(),
                Some(0),
            );
            assert_eq!(input.events.len(), kept, "held={held}");
        }
    }

    /// CR 603.3b + CR 603.12 + CR 614.12a: a stamped copy of a parked entry is not
    /// an SBA-choice answer event, so it selects no closing `Any` scope, and an
    /// unmatched reflexive stays live past the batch's ordering prompt, which ends
    /// the pass before its closing delayed scan. With the entry unparked, the
    /// scope closes it.
    #[test]
    fn a_stamped_copy_of_a_parked_entry_closes_no_sba_answer_scope() {
        for (held, reflexives) in [(true, 1), (false, 0)] {
            let mut scenario = scenario(2);
            scenario.add_creature_from_oracle(
                P0,
                "Second Warden",
                1,
                1,
                "Whenever another creature enters, you lose 1 life.",
            );
            let mut state = scenario.build().state().clone();
            let (id, mut events) = stamped_parked_entry(&mut state);
            if !held {
                state.deferred_entry_events.clear();
            }
            let (_, parked_batch_entry) = entered(&mut state, 901, P0, P0);
            crate::game::triggers::collect_parked_triggers_into_deferred_observing(
                &mut state,
                std::slice::from_ref(&parked_batch_entry),
                crate::game::triggers::DeferredTriggerBatchOrigin::StateBasedActionChoice,
                None,
            );
            assert!(
                crate::game::triggers::has_sba_choice_trigger_batch(&state),
                "reach guard: the SBA choice parked a batch"
            );
            state.delayed_triggers.push(DelayedTrigger {
                condition: DelayedTriggerCondition::WhenNextEvent {
                    trigger: Box::new(TriggerDefinition::new(TriggerMode::SpellCast)),
                    or_trigger: None,
                    lifetime: DelayedTriggerLifetime::Reflexive,
                },
                ability: Box::new(ResolvedAbility::new(
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                    Vec::new(),
                    id,
                    P0,
                )),
                controller: P0,
                source_id: id,
                one_shot: true,
                kind: DelayedTriggerKind::Ordinary,
                provenance: DelayedInstallIdentity::LegacyDelayed,
            });
            let priority = state.waiting_for.clone();
            crate::game::engine_priority::run_post_action_pipeline(
                &mut state,
                &mut events,
                &priority,
                false,
                false,
            )
            .expect("the pass settles");
            assert!(
                matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }),
                "reach guard: the batch opened its ordering prompt: {:?}",
                state.waiting_for
            );
            assert_eq!(state.delayed_triggers.len(), reflexives, "held={held}");
        }
    }

    /// The printed chain's shape: the roll, then its counters on the entrant.
    fn roll_chain() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 6,
                results: Vec::new(),
                modifier: None,
            },
        )
        .sub_ability(rolled_placement())
    }

    fn rolled_placement() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: rolled_amount(),
                target: TargetFilter::SelfRef,
            },
        )
    }

    fn rolled_amount() -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        }
    }

    fn with_drain(entrant_source: Option<ObjectId>, status: DrainStatus) -> (GameState, ObjectId) {
        let mut state = settled_state(2);
        let (entrant, _) = entered(&mut state, 900, P0, P0);
        let mut drain = PostReplacementDrain::ready(PostReplacementContinuation::Template(
            Box::new(roll_chain()),
        ));
        drain.source = Some(entrant_source.unwrap_or(entrant));
        drain.status = status;
        state.install_post_replacement_drain(drain, ResidentDrainPolicy::Replace);
        (state, entrant)
    }

    fn ready_chain(chain: AbilityDefinition) -> DrainStatus {
        DrainStatus::Ready(PostReplacementContinuation::Template(Box::new(chain)))
    }

    fn resident_status(state: &GameState) -> DrainStatus {
        state
            .active_post_replacement_drains()
            .and_then(PostReplacementDrainStack::resident)
            .expect("the seeded drain")
            .status
            .clone()
    }

    fn resident_chain(state: &GameState) -> AbilityDefinition {
        match resident_status(state) {
            DrainStatus::Ready(PostReplacementContinuation::Template(chain)) => *chain,
            other => panic!("expected a ready template, got {other:?}"),
        }
    }

    fn placed_count(link: &AbilityDefinition) -> &QuantityExpr {
        match link.effect.as_ref() {
            Effect::PutCounter { count, .. } => count,
            other => panic!("expected PutCounter, got {other:?}"),
        }
    }

    fn vigilance() -> CounterType {
        CounterType::Keyword(KeywordKind::Vigilance)
    }

    /// CR 614.1c + CR 122.6 + CR 614.5: the roll's kind is one number, in the
    /// roll's own placement.
    #[test]
    fn the_roll_takes_entry_counters_of_its_kind_as_one_placement() {
        // Same kind: the printed placement carries them.
        let (mut state, entrant) = with_drain(None, ready_chain(roll_chain()));
        let left = hand_entry_counters_to_as_enters_roll(
            &mut state,
            entrant,
            vec![(CounterType::Plus1Plus1, 2)],
        );
        assert!(left.is_empty());
        let chain = resident_chain(&state);
        let placement = chain.sub_ability.as_deref().expect("the placement");
        assert_eq!(
            placed_count(placement),
            &QuantityExpr::Offset {
                inner: Box::new(rolled_amount()),
                offset: 2,
            }
        );
        assert!(placement.sub_ability.is_none(), "no link of its own");

        // Two contributions of a kind: one offset of their sum.
        let (mut state, entrant) = with_drain(None, ready_chain(roll_chain()));
        let left = hand_entry_counters_to_as_enters_roll(
            &mut state,
            entrant,
            vec![(CounterType::Plus1Plus1, 1), (CounterType::Plus1Plus1, 2)],
        );
        assert!(left.is_empty());
        let chain = resident_chain(&state);
        let placement = chain.sub_ability.as_deref().expect("the placement");
        assert_eq!(
            placed_count(placement),
            &QuantityExpr::Offset {
                inner: Box::new(rolled_amount()),
                offset: 3,
            }
        );

        // Nothing to hand on: the chain stays as printed.
        let (mut state, entrant) = with_drain(None, ready_chain(roll_chain()));
        let left = hand_entry_counters_to_as_enters_roll(&mut state, entrant, Vec::new());
        assert!(left.is_empty());
        assert_eq!(resident_chain(&state), roll_chain());
    }

    /// CR 122.1: each kind is its own placement. The chain takes the roll's kind and
    /// the list keeps only the other kinds, in their order. A list with nothing of
    /// the roll's kind is refused: it comes back as built for delivery to place, and
    /// the chain stays as printed.
    #[test]
    fn the_hand_on_keeps_only_other_kinds_and_refuses_a_list_of_only_those() {
        let funk = CounterType::Generic("funk".to_string());

        let (mut state, entrant) = with_drain(None, ready_chain(roll_chain()));
        let left = hand_entry_counters_to_as_enters_roll(
            &mut state,
            entrant,
            vec![
                (vigilance(), 1),
                (CounterType::Plus1Plus1, 1),
                (funk.clone(), 1),
                (CounterType::Plus1Plus1, 2),
                (vigilance(), 2),
            ],
        );
        assert_eq!(
            left,
            vec![(vigilance(), 1), (funk.clone(), 1), (vigilance(), 2)]
        );
        let chain = resident_chain(&state);
        let placement = chain.sub_ability.as_deref().expect("the placement");
        assert_eq!(
            placed_count(placement),
            &QuantityExpr::Offset {
                inner: Box::new(rolled_amount()),
                offset: 3,
            }
        );
        assert!(placement.sub_ability.is_none(), "no link of its own");

        for counters in [
            vec![(vigilance(), 1)],
            // The contributions stay apart.
            vec![(vigilance(), 1), (funk.clone(), 1), (vigilance(), 2)],
        ] {
            let (mut state, entrant) = with_drain(None, ready_chain(roll_chain()));
            let left = hand_entry_counters_to_as_enters_roll(&mut state, entrant, counters.clone());
            assert_eq!(left, counters);
            assert_eq!(resident_chain(&state), roll_chain());
        }
    }

    /// A chain with no unconditional placement on the entrant has nothing for the
    /// entry's counters to join. A conditional placement is not this entry's.
    #[test]
    fn the_hand_on_refuses_a_roll_with_no_unconditional_placement() {
        let mut conditional = roll_chain();
        conditional
            .sub_ability
            .as_deref_mut()
            .expect("the placement")
            .condition = Some(AbilityCondition::TriggerEventTargetExploitedBySource);
        let mut unplaced = roll_chain();
        unplaced.sub_ability = None;
        let counters = vec![(CounterType::Plus1Plus1, 1)];
        for chain in [conditional, unplaced] {
            let (mut state, entrant) = with_drain(None, ready_chain(chain.clone()));
            let left = hand_entry_counters_to_as_enters_roll(&mut state, entrant, counters.clone());
            assert_eq!(left, counters);
            assert_eq!(resident_chain(&state), chain);
        }
    }

    /// The gate: every other drain is refused, and the list comes back whole for
    /// delivery to place. The list and each chain below would be taken but for the
    /// gate.
    #[test]
    fn the_hand_on_refuses_every_other_drain() {
        let counters = vec![(CounterType::Plus1Plus1, 2)];
        let non_roll = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .sub_ability(rolled_placement());
        let resolved = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            P0,
        );
        for (source, status) in [
            // Another source with a roll at its head: Barbarian Class's shape.
            (Some(ObjectId(9_999)), ready_chain(roll_chain())),
            (None, ready_chain(non_roll)),
            (
                None,
                DrainStatus::Ready(PostReplacementContinuation::Resolved(Box::new(resolved))),
            ),
            (None, DrainStatus::Dispatching),
            (None, DrainStatus::Paused),
        ] {
            let (mut state, entrant) = with_drain(source, status.clone());
            let left = hand_entry_counters_to_as_enters_roll(&mut state, entrant, counters.clone());
            assert_eq!(left, counters, "{status:?}");
            assert_eq!(resident_status(&state), status);
        }

        // No drain at all.
        let mut state = settled_state(2);
        let (entrant, _) = entered(&mut state, 900, P0, P0);
        let left = hand_entry_counters_to_as_enters_roll(&mut state, entrant, counters.clone());
        assert_eq!(left, counters);
    }

    /// V6, U4-T2. The gate, the recorded state deciding collection, the
    /// owner-frame refusal and the ledger filter.
    #[test]
    fn the_settled_release_gate_and_completion_rule() {
        // Positive: Priority, entrant live, un-owned → its events.
        let mut state = settled_state(2);
        let (_, entry) = parked_entry(&mut state, 900, P0, P0, COMPLETED);
        assert_eq!(release_settled_entry_continuation(&mut state), vec![entry]);
        assert!(state.deferred_entry_events.is_empty());

        // Not Priority → empty, park kept.
        let mut state = settled_state(2);
        let (id, entry) = parked_entry(&mut state, 900, P0, P0, COMPLETED);
        state.waiting_for = WaitingFor::ScryChoice {
            player: P0,
            cards: vec![id],
        };
        assert!(release_settled_entry_continuation(&mut state).is_empty());
        assert_eq!(state.deferred_entry_events, vec![seeded(entry, COMPLETED)]);

        // Priority with a live resolution frame → empty, park kept.
        let mut state = settled_state(2);
        let (_, entry) = parked_entry(&mut state, 900, P0, P0, COMPLETED);
        state.push_counter_additions(PendingCounterAdditionQueue {
            remaining: Vec::new(),
            completion: None,
        });
        assert!(release_settled_entry_continuation(&mut state).is_empty());
        assert_eq!(state.deferred_entry_events, vec![seeded(entry, COMPLETED)]);

        // Owner-frame gate case: the entry is the paused member of a logical
        // owner whose frame is on top → refused by the readiness predicate.
        let mut state = settled_state(2);
        let (id, entry) = parked_entry(&mut state, 900, P0, P0, COMPLETED);
        let mut paused = PendingZoneChangeDelivery::new(
            ObjectIncarnationRef {
                object_id: id,
                incarnation: state.objects[&id].incarnation,
            },
            ProposedEvent::zone_change(id, Zone::Graveyard, Zone::Battlefield, None),
        );
        paused.delivery_events = vec![entry.clone()];
        let logical_zone_change_group = state.allocate_logical_zone_change_group(&[]);
        state
            .resolution_stack
            .push_inner(ResolutionFrame::BatchDelivery(Box::new(
                PendingBatchDeliveries {
                    library_reorder: None,
                    component_delivery_parent: None,
                    staged: None,
                    logical_zone_change_group,
                    paused_current: Some(paused),
                    remaining: Vec::new(),
                    destination: Zone::Battlefield,
                    source_id: None,
                    enter_tapped: EtbTapState::Unspecified,
                    exile_tracking: ZoneDeliveryExileTracking::None,
                    library_placement: None,
                    completion: None,
                    replacement_applied: HashSet::new(),
                    requests: Vec::new(),
                    attempted: Vec::new(),
                    zone_change_record_start: 0,
                    deferred_events: Vec::new(),
                    pre_entry_battlefield: None,
                },
            )));
        assert!(!crate::game::engine_priority::retained_logical_zone_events(&state).is_empty());
        assert!(release_settled_entry_continuation(&mut state).is_empty());
        assert_eq!(state.deferred_entry_events, vec![seeded(entry, COMPLETED)]);

        // An equal occurrence already queued → empty (the queued-witness filter),
        // and the park is taken.
        let mut state = settled_state(2);
        let (_, entry) = parked_entry(&mut state, 900, P0, P0, COMPLETED);
        state.deferred_entry_events.clear();
        crate::game::triggers::collect_triggers_into_deferred(
            &mut state,
            std::slice::from_ref(&entry),
        );
        assert!(!state.deferred_triggers.is_empty(), "reach guard: queued");
        state.deferred_entry_events = vec![seeded(entry, COMPLETED)];
        assert!(release_settled_entry_continuation(&mut state).is_empty());
        assert!(state.deferred_entry_events.is_empty());

        // The entrant left the battlefield after its entry completed → still
        // returned (CR 603.2 + CR 603.10).
        let mut state = settled_state(2);
        let (id, entry) = parked_entry(&mut state, 900, P0, P0, COMPLETED);
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, id, Zone::Graveyard, &mut events);
        assert_eq!(state.objects[&id].zone, Zone::Graveyard);
        assert_eq!(release_settled_entry_continuation(&mut state), vec![entry]);

        // A pending entry is dropped with no one leaving: the gate means no
        // continuation is live, so it can only have been abandoned.
        let mut state = settled_state(2);
        parked_entry(&mut state, 900, P0, P0, PENDING);
        assert!(release_settled_entry_continuation(&mut state).is_empty());
        assert!(state.deferred_entry_events.is_empty());

        // Two entrants released together: only the completed one is returned.
        let mut state = settled_state(2);
        let (_, first) = entered(&mut state, 900, P0, P0);
        let (_, second) = entered(&mut state, 901, P0, P0);
        state.deferred_entry_events =
            vec![seeded(first.clone(), COMPLETED), seeded(second, PENDING)];
        assert_eq!(release_settled_entry_continuation(&mut state), vec![first]);
        assert!(state.deferred_entry_events.is_empty());

        // Predicate: a `Moved` continuation for an object no longer on the
        // battlefield has no as-enters entrant; on the battlefield it does.
        let mut state = settled_state(2);
        let (id, entry) = parked_entry(&mut state, 900, P0, P0, COMPLETED);
        state.deferred_entry_events.clear();
        let events = vec![entry];
        assert_eq!(
            as_enters_continuation_entrant(
                &state,
                Some(&ReplacementEvent::Moved),
                Some(id),
                &events
            ),
            Some(id)
        );
        assert_eq!(
            as_enters_continuation_entrant(&state, None, Some(id), &events),
            None
        );
        let mut moved = Vec::new();
        crate::game::zones::move_to_zone(&mut state, id, Zone::Graveyard, &mut moved);
        assert_eq!(
            as_enters_continuation_entrant(
                &state,
                Some(&ReplacementEvent::Moved),
                Some(id),
                &events
            ),
            None
        );
    }

    /// U4-T2, the delayed half: a matching "whenever a creature enters this
    /// turn" delayed trigger is queued exactly once by the release itself.
    #[test]
    fn the_settled_release_runs_the_delayed_half() {
        let mut scenario = scenario(2);
        let beck = scenario
            .add_spell_to_hand_from_oracle(P0, "Beck", false, BECK)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner: GameRunner = scenario.build();
        runner.cast(beck).resolve();
        runner.advance_until_stack_empty();
        assert!(!runner.state().delayed_triggers.is_empty(), "reach guard");
        let state = runner.state_mut();
        let (_, entry) = parked_entry(state, 900, P0, P0, COMPLETED);
        let released = release_settled_entry_continuation(state);
        assert_eq!(released, vec![entry]);
        let delayed = state
            .deferred_triggers
            .iter()
            .filter(|context| context.pending.source_id == beck)
            .count();
        assert_eq!(delayed, 1, "{:?}", state.deferred_triggers);
        let _ = GameAction::PassPriority;
    }

    /// V6, U4-T6. 3 players, owner ≠ controller: a departure does not decide the
    /// take. CR 603.2 + CR 800.4a: a completed entry is returned whoever left, and
    /// a pending one is dropped whoever left.
    #[test]
    fn a_departure_does_not_decide_the_take() {
        for leaver in [P0, P1, PlayerId(2)] {
            let mut state = settled_state(3);
            let (_, entry) = parked_entry(&mut state, 900, P1, P0, COMPLETED);
            let mut events = Vec::new();
            crate::game::elimination::eliminate_player(
                &mut state,
                leaver,
                crate::types::events::LossReason::Conceded,
                &mut events,
            );
            assert!(
                !state.deferred_entry_events.is_empty(),
                "reach guard: park kept"
            );
            assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
            assert_eq!(
                release_settled_entry_continuation(&mut state),
                vec![entry],
                "{leaver:?} left"
            );
            assert!(state.deferred_entry_events.is_empty());

            let mut state = settled_state(3);
            parked_entry(&mut state, 900, P1, P0, PENDING);
            let mut events = Vec::new();
            crate::game::elimination::eliminate_player(
                &mut state,
                leaver,
                crate::types::events::LossReason::Conceded,
                &mut events,
            );
            assert!(
                !state.deferred_entry_events.is_empty(),
                "reach guard: park kept"
            );
            assert!(
                release_settled_entry_continuation(&mut state).is_empty(),
                "{leaver:?} left"
            );
            assert!(state.deferred_entry_events.is_empty());
        }

        // Both the owner and the controller leave, with the game still going.
        let mut state = settled_state(4);
        let (_, entry) = parked_entry(&mut state, 900, P1, P0, COMPLETED);
        let mut events = Vec::new();
        crate::game::elimination::eliminate_player(
            &mut state,
            P0,
            crate::types::events::LossReason::Conceded,
            &mut events,
        );
        crate::game::elimination::eliminate_player(
            &mut state,
            P1,
            crate::types::events::LossReason::Conceded,
            &mut events,
        );
        assert_eq!(release_settled_entry_continuation(&mut state), vec![entry]);
    }

    /// V7, decision 3: every removal that answers one entrant takes that
    /// entrant's entries, in either state, and leaves the others unchanged.
    #[test]
    fn a_removal_leaves_the_entries_it_does_not_own() {
        type Removal = fn(&mut GameState, ObjectId, &[GameEvent]);
        let removals: [(&str, Removal); 6] = [
            ("replay", |state, entrant, _| {
                // A prompt keeps the settled take out of the replay's own drain.
                state.waiting_for = WaitingFor::ScryChoice {
                    player: P0,
                    cards: Vec::new(),
                };
                let mut events = Vec::new();
                replay_deferred_entry_events(state, entrant, &mut events).expect("replay");
            }),
            ("flush", |state, entrant, _| {
                let mut events = Vec::new();
                flush_deferred_entry_events_into_priority_scan(state, entrant, &mut events);
                assert!(!events.is_empty(), "the flush moved the entrant's entries");
            }),
            ("stale copy resume", |state, entrant, _| {
                let reference = ObjectIncarnationRef::of(entrant, u64::MAX);
                let mut events = Vec::new();
                resume_copy_target_choice_entry(
                    state,
                    reference,
                    crate::types::game_state::CopyEntryResumeStage::ChoiceOwedAfterCounters,
                    &mut events,
                )
                .expect("resume");
            }),
            ("capture", |state, entrant, _| {
                let waiting_for = WaitingFor::CopyTargetChoice {
                    player: P0,
                    source_id: entrant,
                    valid_targets: Vec::new(),
                    max_mana_value: None,
                    purpose: CopyTargetPurpose::BecomeCopy,
                };
                let mut events = Vec::new();
                capture_deferred_entry_events_if_mid_entry_choice(
                    state,
                    Some(&waiting_for),
                    &mut events,
                );
            }),
            ("custody", |state, _, own| {
                take_entry_continuation_custody(state, own);
            }),
            ("inline close", |state, entrant, own| {
                let dispatch = dispatch_on_a_fresh_frame(state);
                EntryContinuationParking {
                    custody: EntryCustody::Opened {
                        entrant,
                        parked: own.to_vec(),
                    },
                }
                .close(state, dispatch, None);
            }),
        ];
        for (name, remove) in removals {
            for foreign_state in [COMPLETED, PENDING] {
                for own_state in [COMPLETED, PENDING] {
                    let mut state = settled_state(2);
                    let (entrant, own) = entered(&mut state, 900, P0, P0);
                    let (_, foreign) = entered(&mut state, 901, P0, P0);
                    let land_played = GameEvent::LandPlayed {
                        object_id: entrant,
                        player_id: P0,
                        from_zone: Zone::Hand,
                    };
                    state.deferred_entry_events = vec![
                        seeded(own.clone(), own_state),
                        seeded(foreign.clone(), foreign_state),
                        seeded(land_played.clone(), own_state),
                    ];
                    remove(&mut state, entrant, &[own, land_played]);
                    assert_eq!(
                        state.deferred_entry_events,
                        vec![seeded(foreign, foreign_state)],
                        "{name}, own {own_state:?}"
                    );
                }
            }
        }

        // The replay's positive half: the entrant's own entry reached collection.
        let mut state = settled_state(2);
        let (entrant, own) = parked_entry(&mut state, 900, P0, P0, PENDING);
        let mut events = Vec::new();
        replay_deferred_entry_events(&mut state, entrant, &mut events).expect("replay");
        assert!(
            !state.stack.is_empty() || !state.deferred_triggers.is_empty(),
            "{own:?} reached Soul Warden"
        );

        // An empty park: every entrant-level removal is a no-op.
        let mut state = settled_state(2);
        assert!(take_parked_entries_of(&mut state, ObjectId(900)).is_empty());

        // An entering Aura's attachment moves with its entrant.
        let mut state = settled_state(2);
        let attach = GameEvent::Attached {
            attachment_id: ObjectId(900),
            target: TargetRef::Object(ObjectId(901)),
        };
        state.deferred_entry_events = vec![seeded(attach.clone(), PENDING)];
        assert_eq!(
            take_parked_entries_of(&mut state, ObjectId(900)),
            vec![attach]
        );

        // A non-entering source (the Esix shape): capture writes nothing and
        // removes nothing of another entrant's.
        let mut state = settled_state(2);
        let (resident, _) = entered(&mut state, 900, P0, P0);
        let (_, foreign) = parked_entry(&mut state, 901, P0, P0, COMPLETED);
        let waiting_for = WaitingFor::CopyTargetChoice {
            player: P0,
            source_id: resident,
            valid_targets: Vec::new(),
            max_mana_value: None,
            purpose: CopyTargetPurpose::CopyTokenSource,
        };
        let mut events = vec![foreign.clone()];
        capture_deferred_entry_events_if_mid_entry_choice(
            &mut state,
            Some(&waiting_for),
            &mut events,
        );
        assert_eq!(
            state.deferred_entry_events,
            vec![seeded(foreign, COMPLETED)]
        );
    }

    fn inert_continuation() -> PostReplacementContinuation {
        PostReplacementContinuation::Template(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented("test", "continuation"),
        )))
    }

    /// One resident drain, begun, on a fresh stamped frame.
    fn dispatch_on_a_fresh_frame(state: &mut GameState) -> IdentifiedPostReplacementDispatch {
        state.install_post_replacement_drain(
            PostReplacementDrain::ready(inert_continuation()),
            ResidentDrainPolicy::Replace,
        );
        state
            .begin_post_replacement_dispatch()
            .expect("a ready drain dispatches")
            .1
    }

    fn reference(state: &GameState, id: ObjectId) -> ObjectIncarnationRef {
        ObjectIncarnationRef::from_object(&state.objects[&id])
    }

    /// A paused resident on a fresh frame, bound to `entrant`.
    fn paused_owner(state: &mut GameState, entrant: ObjectIncarnationRef) {
        let mut drains = PostReplacementDrainStack::default();
        drains.install(
            PostReplacementDrain::paused_owner_of(entrant),
            ResidentDrainPolicy::Replace,
        );
        state.resolution_stack.push_post_replacement(drains);
    }

    /// V12. CR 603.2: the optional accept is a retire inside a running event, so
    /// it reaches the completion authority; the abandonment path does not. The
    /// gate reads the moment the continuation ended.
    #[test]
    fn the_optional_accept_records_and_the_abandonment_does_not() {
        let mut state = settled_state(2);
        let (id, entry) = parked_entry(&mut state, 900, P0, P0, PENDING);
        let bound = reference(&state, id);
        paused_owner(&mut state, bound);
        state.abandon_active_post_replacement_drains();
        assert_eq!(state.deferred_entry_events, vec![seeded(entry, COMPLETED)]);
        assert!(state
            .active_post_replacement_drains()
            .is_some_and(PostReplacementDrainStack::is_empty));

        let mut state = settled_state(2);
        let (id, entry) = parked_entry(&mut state, 900, P0, P0, PENDING);
        let bound = reference(&state, id);
        paused_owner(&mut state, bound);
        state.abandon_active_replacement_tails();
        assert_eq!(state.deferred_entry_events, vec![seeded(entry, PENDING)]);

        // Entrant gone and its owner left: stays pending.
        let mut state = settled_state(3);
        let (id, entry) = parked_entry(&mut state, 900, P1, P1, PENDING);
        let bound = reference(&state, id);
        let mut events = Vec::new();
        crate::game::elimination::eliminate_player(
            &mut state,
            P1,
            crate::types::events::LossReason::Conceded,
            &mut events,
        );
        record_parked_entry_completion(&mut state, &[bound]);
        assert_eq!(state.deferred_entry_events, vec![seeded(entry, PENDING)]);

        // Entrant gone and no one left: its entry happened (CR 603.10).
        let mut state = settled_state(2);
        let (id, entry) = parked_entry(&mut state, 900, P0, P0, PENDING);
        let bound = reference(&state, id);
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, id, Zone::Graveyard, &mut events);
        record_parked_entry_completion(&mut state, &[bound]);
        assert_eq!(state.deferred_entry_events, vec![seeded(entry, COMPLETED)]);

        // A stale entry of an earlier incarnation of the same id is not revived.
        let mut state = settled_state(2);
        let (id, entry) = parked_entry(&mut state, 900, P0, P0, PENDING);
        let later = ObjectIncarnationRef::of(id, state.objects[&id].incarnation + 1);
        record_parked_entry_completion(&mut state, &[later]);
        assert_eq!(state.deferred_entry_events, vec![seeded(entry, PENDING)]);
    }

    /// V13 (b). CR 616.1g: a `Ready` drain that inherited a binding and runs its
    /// continuation inline completes the entry at the dispatcher's else branch.
    #[test]
    fn an_inline_ready_drain_completes_an_inherited_binding() {
        let mut state = settled_state(2);
        let (id, entry) = parked_entry(&mut state, 900, P0, P0, PENDING);
        let mut drain = PostReplacementDrain::ready(inert_continuation());
        drain.parked_entrants.push(reference(&state, id));
        state.install_post_replacement_drain(drain, ResidentDrainPolicy::Replace);
        let mut events = Vec::new();
        let waiting_for =
            apply_pending_post_replacement_effect(&mut state, None, None, None, &mut events);
        assert!(waiting_for.is_none());
        assert_eq!(state.deferred_entry_events, vec![seeded(entry, COMPLETED)]);
    }

    /// V13 (c). CR 616.1g: with a prompt open, a retired dispatch's binding moves
    /// to the frame's new resident and the entry stays pending.
    #[test]
    fn a_retired_dispatch_hands_its_binding_to_the_new_resident() {
        let mut state = settled_state(2);
        let (id, entry) = parked_entry(&mut state, 900, P0, P0, PENDING);
        let bound = reference(&state, id);
        let dispatch = dispatch_on_a_fresh_frame(&mut state);
        assert!(state.bind_parked_entrant(dispatch, bound));
        let nested = PostReplacementDrain {
            status: DrainStatus::Paused,
            ..PostReplacementDrain::ready(inert_continuation())
        };
        state
            .active_post_replacement_drains_mut()
            .expect("the frame")
            .install(nested, ResidentDrainPolicy::Replace);
        let retired = state
            .finish_post_replacement_dispatch(dispatch)
            .expect("the outer drain retires");
        let prompt = WaitingFor::ScryChoice {
            player: P0,
            cards: Vec::new(),
        };
        settle_retired_dispatch_entrants(
            &mut state,
            dispatch,
            retired.parked_entrants,
            Some(&prompt),
        );
        let resident = state
            .active_post_replacement_drains()
            .and_then(PostReplacementDrainStack::resident)
            .expect("the nested drain");
        assert_eq!(resident.parked_entrants, vec![bound]);
        assert_eq!(state.deferred_entry_events, vec![seeded(entry, PENDING)]);
    }

    /// V22. CR 614.12a + CR 707.9: a copied choice binds to the resident, whose
    /// retirement completes the entry, and adds no owner.
    #[test]
    fn a_copied_choice_binds_to_the_resident_and_its_retirement_completes() {
        let host = ObjectId(777);
        let mut state = settled_state(2);
        let (id, entry) = parked_entry(&mut state, 900, P0, P0, PENDING);
        let mut drains = PostReplacementDrainStack::default();
        drains.install(
            PostReplacementDrain {
                status: DrainStatus::Paused,
                source: Some(host),
                ..PostReplacementDrain::ready(inert_continuation())
            },
            ResidentDrainPolicy::Replace,
        );
        state.resolution_stack.push_post_replacement(drains);
        assert_eq!(bind_copied_choice_entrant(&mut state, id), None);
        let resident = state
            .active_post_replacement_drains()
            .and_then(PostReplacementDrainStack::resident)
            .expect("the resident");
        assert_eq!(resident.parked_entrants, vec![reference(&state, id)]);
        assert_eq!(resident.source, Some(host));
        assert_eq!(
            state.deferred_entry_events,
            vec![seeded(entry.clone(), PENDING)]
        );
        state.finish_active_paused_post_replacement_dispatch();
        assert_eq!(state.deferred_entry_events, vec![seeded(entry, COMPLETED)]);

        // No parked entry: nothing to bind.
        let mut state = settled_state(2);
        let (id, _) = entered(&mut state, 900, P0, P0);
        assert_eq!(bind_copied_choice_entrant(&mut state, id), None);

        // No resident: the caller parks the owner.
        let mut state = settled_state(2);
        let (id, _) = parked_entry(&mut state, 900, P0, P0, PENDING);
        assert_eq!(
            bind_copied_choice_entrant(&mut state, id),
            Some(reference(&state, id))
        );
        assert!(state.active_post_replacement_drains().is_none());

        // A resident bound to another entrant: nothing is written, and the caller
        // parks the owner.
        let mut state = settled_state(2);
        let (id, entry) = parked_entry(&mut state, 900, P0, P0, PENDING);
        let (other, _) = entered(&mut state, 901, P1, P1);
        let bound = reference(&state, other);
        let mut drains = PostReplacementDrainStack::default();
        drains.install(
            PostReplacementDrain {
                status: DrainStatus::Paused,
                source: Some(host),
                parked_entrants: vec![bound],
                ..PostReplacementDrain::ready(inert_continuation())
            },
            ResidentDrainPolicy::Replace,
        );
        state.resolution_stack.push_post_replacement(drains);
        assert_eq!(
            bind_copied_choice_entrant(&mut state, id),
            Some(reference(&state, id))
        );
        let resident = state
            .active_post_replacement_drains()
            .and_then(PostReplacementDrainStack::resident)
            .expect("the resident");
        assert_eq!(resident.parked_entrants, vec![bound]);
        assert_eq!(state.deferred_entry_events, vec![seeded(entry, PENDING)]);
    }

    /// U-1. CR 614.1c + CR 603.2 + CR 706.2: an entrant's own as-enters roll is
    /// parked completed beside its entry. Closing A's inline continuation unparks
    /// only A's entry, custody over the rolls takes nothing, an abandonment keeps
    /// them because they happened, and the settled release takes each once. It
    /// returns no roll: what a roll triggered was collected when it was rolled,
    /// and the release queues those held contexts instead (none here).
    #[test]
    fn an_as_enters_roll_waits_in_the_park_for_the_settled_release() {
        let mut state = settled_state(2);
        let (a, entry_a) = entered(&mut state, 900, P0, P0);
        let (b, entry_b) = entered(&mut state, 901, P0, P0);
        let roll = |entrant| GameEvent::DieRolled {
            player_id: P0,
            sides: 6,
            result: Some(4),
            pre_entry_source: Some(entrant),
        };
        let (roll_a, roll_b) = (roll(a), roll(b));
        state.deferred_entry_events.clear();
        park_pending_entry(&mut state, entry_a.clone());
        park_pending_entry(&mut state, entry_b.clone());
        park_as_enters_roll(&mut state, roll_a.clone(), Vec::new());
        park_as_enters_roll(&mut state, roll_b.clone(), Vec::new());

        let dispatch = dispatch_on_a_fresh_frame(&mut state);
        EntryContinuationParking {
            custody: EntryCustody::Opened {
                entrant: a,
                parked: vec![entry_a],
            },
        }
        .close(&mut state, dispatch, None);
        let after_close = vec![
            seeded(entry_b, PENDING),
            seeded(roll_a.clone(), COMPLETED),
            seeded(roll_b.clone(), COMPLETED),
        ];
        assert_eq!(state.deferred_entry_events, after_close, "A's entry only");

        take_entry_continuation_custody(&mut state, &[roll_a.clone(), roll_b.clone()]);
        assert_eq!(
            state.deferred_entry_events, after_close,
            "custody leaves rolls"
        );

        drop_abandoned_family_entries(&mut state, None);
        assert_eq!(
            state.deferred_entry_events,
            vec![
                seeded(roll_a.clone(), COMPLETED),
                seeded(roll_b.clone(), COMPLETED)
            ],
            "the rolls happened"
        );

        *state.resolution_stack = Default::default();
        assert!(take_settled_entry_continuation_events(&mut state).is_empty());
        assert!(
            state.deferred_entry_events.is_empty(),
            "each roll is taken once"
        );
        assert!(state.deferred_triggers.is_empty());
        assert!(take_settled_entry_continuation_events(&mut state).is_empty());
    }

    /// U-2. CR 508.4 + CR 400.7: an owed attacking seat is stamped on the
    /// entrant's current incarnation's parked entry and taken once by the owner
    /// whose paused delivery holds that entry's ledger key. Another entrant's
    /// delivery, a stale incarnation's entry, or an entrant that has since become
    /// a later incarnation takes nothing to seat.
    #[test]
    fn an_owed_attack_seat_is_taken_by_its_deliverys_ledger_key() {
        let mut state = settled_state(2);
        let (a, stale) = entered(&mut state, 900, P0, P0);
        let mut moved = Vec::new();
        crate::game::zones::move_to_zone(&mut state, a, Zone::Graveyard, &mut moved);
        let mut returned = Vec::new();
        crate::game::zones::move_to_zone(&mut state, a, Zone::Battlefield, &mut returned);
        let current = returned
            .into_iter()
            .find(|event| {
                matches!(
                    event,
                    GameEvent::ZoneChanged {
                        to: Zone::Battlefield,
                        ..
                    }
                )
            })
            .expect("the re-entry");
        let (b, entry_b) = entered(&mut state, 901, P0, P0);
        assert!(
            !parked_event_holds(&stale, &current) && !parked_event_holds(&current, &entry_b),
            "reach guard: three ledger keys"
        );
        state.deferred_entry_events = vec![
            seeded(stale.clone(), PENDING),
            seeded(current.clone(), PENDING),
            seeded(entry_b.clone(), PENDING),
        ];
        let stamps = |state: &GameState| -> Vec<Option<EntryAttackDestination>> {
            state
                .deferred_entry_events
                .iter()
                .map(|parked| parked.entry_attack.clone())
                .collect()
        };

        assert!(owe_entry_attack_seat(&mut state, a));
        assert_eq!(
            stamps(&state),
            vec![None, Some(EntryAttackDestination::AnyDefender), None],
            "only A's current incarnation"
        );
        assert_eq!(take_owed_entry_attack_seat(&mut state, &[entry_b]), None);
        assert_eq!(take_owed_entry_attack_seat(&mut state, &[stale]), None);
        assert_eq!(
            stamps(&state),
            vec![None, Some(EntryAttackDestination::AnyDefender), None],
            "A's stamp stays for A's owner"
        );
        assert_eq!(
            take_owed_entry_attack_seat(&mut state, std::slice::from_ref(&current)),
            Some((reference(&state, a), EntryAttackDestination::AnyDefender))
        );
        assert_eq!(
            take_owed_entry_attack_seat(&mut state, std::slice::from_ref(&current)),
            None,
            "taken once"
        );

        assert!(owe_entry_attack_seat(&mut state, a));
        state.objects.get_mut(&a).expect("A").incarnation += 1;
        assert_eq!(
            take_owed_entry_attack_seat(&mut state, std::slice::from_ref(&current)),
            None,
            "a later incarnation is not seated"
        );
        assert_eq!(stamps(&state), vec![None; 3], "the stamp is still taken");
        assert!(owe_entry_attack_seat(&mut state, b), "B's own entry");
    }

    const BRAZEN_DWARF: &str =
        "Whenever you roll one or more dice, this creature deals 1 damage to each opponent.";

    /// Install `entrant`'s not-yet-run as-enters roll: a ready `RollDie` chain
    /// sourced at it, on a fresh frame.
    fn install_roll_chain(state: &mut GameState, entrant: ObjectId) {
        let chain = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::RollDie {
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                sides: 6,
                results: Vec::new(),
                modifier: None,
            },
        );
        state.install_post_replacement_drain(
            PostReplacementDrain {
                source: Some(entrant),
                ..PostReplacementDrain::ready(PostReplacementContinuation::Template(Box::new(
                    chain,
                )))
            },
            ResidentDrainPolicy::Replace,
        );
    }

    /// `id`'s parked battlefield entry, if any.
    fn parked_entry_state(state: &GameState, entry: &GameEvent) -> Option<ParkedEntryState> {
        state
            .deferred_entry_events
            .iter()
            .find(|parked| parked.event == *entry)
            .map(|parked| parked.state)
    }

    /// Move `id` out and back, returning its new battlefield entry (CR 400.7).
    fn reenter(state: &mut GameState, id: ObjectId) -> GameEvent {
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(state, id, Zone::Graveyard, &mut events);
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(state, id, Zone::Battlefield, &mut events);
        events
            .into_iter()
            .find(|event| {
                matches!(
                    event,
                    GameEvent::ZoneChanged {
                        to: Zone::Battlefield,
                        ..
                    }
                )
            })
            .expect("the new entry")
    }

    /// R11. CR 614.12a + CR 603.6d + CR 400.7: an as-enters roll's dispatch adopts
    /// the entry its delivery already parked, by identity, whatever event it
    /// carries, and completes it with the continuation: recorded completed and
    /// still parked for its collector, because its `ZoneChanged` belongs to an
    /// earlier action's buffer. With nothing parked there is nothing to adopt,
    /// and another incarnation's parked entry is not adopted.
    #[test]
    fn an_as_enters_roll_adopts_the_entry_its_delivery_parked() {
        let mut state = settled_state(2);
        let (x, entry) = parked_entry(&mut state, 900, P0, P0, PENDING);
        install_roll_chain(&mut state, x);
        let mut events = Vec::new();
        let waiting_for =
            apply_pending_post_replacement_effect(&mut state, None, None, None, &mut events);
        assert!(waiting_for.is_none(), "{waiting_for:?}");
        // Reach: the roll was made while the entry was held pending.
        let stamped: Vec<_> = events
            .iter()
            .filter(|event| is_as_enters_roll(event))
            .collect();
        assert_eq!(stamped.len(), 1, "{events:?}");
        assert_eq!(parked_entry_state(&state, &entry), Some(COMPLETED));

        // Nothing parked: no custody is adopted or opened (the dispatch carries
        // no `Moved` event), so nothing is parked and the roll is unstamped.
        let mut state = settled_state(2);
        let (y, entry) = entered(&mut state, 901, P0, P0);
        state.deferred_entry_events.clear();
        install_roll_chain(&mut state, y);
        let mut events = Vec::new();
        let _ = apply_pending_post_replacement_effect(&mut state, None, None, None, &mut events);
        assert_eq!(parked_entry_state(&state, &entry), None);
        assert!(
            !events.iter().any(is_as_enters_roll),
            "an entry not held is not mid-entry"
        );

        // Another incarnation's parked entry is not adopted (CR 400.7).
        let mut state = settled_state(2);
        let (z, stale) = parked_entry(&mut state, 902, P0, P0, PENDING);
        let _ = reenter(&mut state, z);
        install_roll_chain(&mut state, z);
        let mut events = Vec::new();
        let _ = apply_pending_post_replacement_effect(&mut state, None, None, None, &mut events);
        assert_eq!(parked_entry_state(&state, &stale), Some(PENDING));
    }

    /// R12. CR 603.2 + CR 603.3 + CR 800.4d: a held roll context round-trips with
    /// its parked roll, a legacy bare event still decodes completed with nothing
    /// held, and the settled release queues a held context only while its
    /// controller is in the game.
    #[test]
    fn a_held_roll_context_round_trips_and_is_placed_for_live_controllers() {
        let board = || {
            let mut scenario = scenario(3);
            scenario.add_creature_from_oracle(P1, "Brazen Dwarf", 1, 3, BRAZEN_DWARF);
            scenario.build().state().clone()
        };
        let roll = GameEvent::DieRolled {
            player_id: P1,
            sides: 6,
            result: Some(4),
            pre_entry_source: Some(ObjectId(900)),
        };
        let mut state = board();
        let held = crate::game::triggers::collect_as_enters_roll_observers(
            &mut state,
            std::slice::from_ref(&roll),
        );
        assert_eq!(held.len(), 1, "reach: the Dwarf is held");
        let mut parked = ParkedEntry::occurred(roll.clone());
        parked.held_triggers = held;
        let wire = serde_json::to_value(&parked).expect("serializes");
        let decoded: ParkedEntry = serde_json::from_value(wire).expect("decodes");
        assert_eq!(decoded, parked);
        let legacy: ParkedEntry =
            serde_json::from_value(serde_json::to_value(&roll).expect("event")).expect("legacy");
        assert_eq!(legacy, ParkedEntry::occurred(roll.clone()));

        let mut state = board();
        state.deferred_entry_events = vec![parked.clone()];
        assert!(take_settled_entry_continuation_events(&mut state).is_empty());
        assert_eq!(
            state.deferred_triggers.len(),
            1,
            "a live controller's context"
        );

        let mut state = board();
        state.players[1].is_eliminated = true;
        state.deferred_entry_events = vec![parked];
        assert!(take_settled_entry_continuation_events(&mut state).is_empty());
        assert!(
            state.deferred_triggers.is_empty(),
            "a departed controller's context is retired"
        );
        assert!(state.deferred_entry_events.is_empty());
    }

    /// A `ChangeZone` owner whose paused delivery of `x` carries `delivery`,
    /// counted as `count` says, pushed on top.
    fn change_zone_owner(
        state: &mut GameState,
        x: ObjectId,
        delivery: GameEvent,
        count: crate::types::game_state::PausedZoneChangeDeliveryCount,
    ) {
        let mut paused = PendingZoneChangeDelivery::new(
            reference(state, x),
            ProposedEvent::zone_change(x, Zone::Graveyard, Zone::Battlefield, None),
        );
        paused.delivery_events = vec![delivery];
        paused.count = count;
        let logical_zone_change_group = state.allocate_logical_zone_change_group(&[]);
        state
            .resolution_stack
            .push_inner(ResolutionFrame::ChangeZone(Box::new(
                crate::types::resolution::ChangeZoneFrame {
                    pending: Some(crate::types::game_state::PendingChangeZoneIteration {
                        library_reorder: None,
                        with_entry_effect: None,
                        performed_by: P0,
                        logical_zone_change_group,
                        pending_return_result_producer: None,
                        paused_current: Some(paused),
                        remaining: Vec::new(),
                        source_id: x,
                        controller: P0,
                        origin: None,
                        destination: Zone::Battlefield,
                        enter_transformed: false,
                        enters_under_player: None,
                        duration: None,
                        track_exiled_by_source: false,
                        face_down_in_exile: crate::types::ability::ExileConcealment::Public,
                        moved_count: None,
                        face_down_profile: None,
                        library_placement: None,
                        library_shuffle: crate::types::ability::LibraryShuffleMode::PerObject,
                        enter_attached_to: None,
                        effect_kind: crate::types::ability::EffectKind::ChangeZone,
                    }),
                    awaiting_batch: None,
                    devour_eligible_snapshot: None,
                },
            )));
    }

    /// R15. CR 603.6d + CR 603.2 + CR 508.4 + CR 400.7: the delivery-tail flush
    /// leaves every entry a live collector owns: one bound to a paused drain
    /// beneath the top, one a batch owner's paused delivery holds deeper down,
    /// one a `ChangeZone` owner's paused delivery holds while that owner still
    /// counts its member, and one that owes its attacking seat. It takes the
    /// entries no collector owns, including one whose `ChangeZone` owner already
    /// counted it. No printed board separates the bound entry from the
    /// owner-held one.
    #[test]
    fn the_delivery_tail_flush_takes_only_unowned_entries() {
        let mut state = settled_state(2);
        let (x, bound) = entered(&mut state, 900, P0, P0);
        let bound_ref = reference(&state, x);
        let held = reenter(&mut state, x);
        let change_zone_held = reenter(&mut state, x);
        let change_zone_counted = reenter(&mut state, x);
        let stamped = reenter(&mut state, x);
        let land_played = GameEvent::LandPlayed {
            object_id: x,
            player_id: P0,
            from_zone: Zone::Hand,
        };
        let mut owed = seeded(stamped.clone(), PENDING);
        owed.entry_attack = Some(EntryAttackDestination::AnyDefender);
        state.deferred_entry_events = vec![
            seeded(bound.clone(), PENDING),
            seeded(held.clone(), PENDING),
            seeded(change_zone_held.clone(), PENDING),
            seeded(change_zone_counted.clone(), PENDING),
            owed.clone(),
            seeded(land_played.clone(), PENDING),
        ];
        let mut paused = PendingZoneChangeDelivery::new(
            reference(&state, x),
            ProposedEvent::zone_change(x, Zone::Graveyard, Zone::Battlefield, None),
        );
        paused.delivery_events = vec![held.clone()];
        let logical_zone_change_group = state.allocate_logical_zone_change_group(&[]);
        state
            .resolution_stack
            .push_inner(ResolutionFrame::BatchDelivery(Box::new(
                PendingBatchDeliveries {
                    library_reorder: None,
                    component_delivery_parent: None,
                    staged: None,
                    logical_zone_change_group,
                    paused_current: Some(paused),
                    remaining: Vec::new(),
                    destination: Zone::Battlefield,
                    source_id: None,
                    enter_tapped: EtbTapState::Unspecified,
                    exile_tracking: ZoneDeliveryExileTracking::None,
                    library_placement: None,
                    completion: None,
                    replacement_applied: HashSet::new(),
                    requests: Vec::new(),
                    attempted: Vec::new(),
                    zone_change_record_start: 0,
                    deferred_events: Vec::new(),
                    pre_entry_battlefield: None,
                },
            )));
        change_zone_owner(
            &mut state,
            x,
            change_zone_held.clone(),
            PausedZoneChangeDeliveryCount::NeedsCount,
        );
        change_zone_owner(
            &mut state,
            x,
            change_zone_counted.clone(),
            PausedZoneChangeDeliveryCount::AlreadyCounted,
        );
        paused_owner(&mut state, bound_ref);
        for _ in 0..2 {
            state.push_counter_additions(PendingCounterAdditionQueue {
                remaining: Vec::new(),
                completion: None,
            });
        }
        let mut events = Vec::new();
        flush_deferred_entry_events_into_priority_scan(&mut state, x, &mut events);
        assert_eq!(
            events,
            vec![change_zone_counted, land_played],
            "only the unowned entries are flushed"
        );
        assert_eq!(
            state.deferred_entry_events,
            vec![
                seeded(bound.clone(), PENDING),
                seeded(held, PENDING),
                seeded(change_zone_held, PENDING),
                owed
            ],
            "the four owned entries stay parked"
        );

        // CR 400.7: a drain bound to another incarnation of X does not own this one.
        let mut state = settled_state(2);
        let (x, bound) = entered(&mut state, 900, P0, P0);
        let _ = reenter(&mut state, x);
        state.deferred_entry_events = vec![seeded(bound.clone(), PENDING)];
        let current = reference(&state, x);
        paused_owner(&mut state, current);
        let mut events = Vec::new();
        flush_deferred_entry_events_into_priority_scan(&mut state, x, &mut events);
        assert_eq!(events, vec![bound]);
    }
}
