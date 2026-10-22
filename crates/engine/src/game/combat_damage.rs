use crate::game::combat::{AttackTarget, CombatState, DamageAssignment, DamageTarget, TrampleKind};
use crate::game::effects::deal_damage::{
    apply_damage_after_replacement, emit_or_accumulate_protection_prevention,
    pre_replacement_damage_gate, tally_entries_for_record, tally_map_from_record_entries,
    DamageContext, DamageGateOutcome, DamageResult,
};
use crate::game::effects::life::ReplacementDeferred;
use crate::game::game_object::GameObject;
use crate::game::replacement;
use crate::game::sba;
use crate::game::triggers;
use crate::game::turns;
use crate::types::ability::{DamageContextSnapshot, ShieldKind, TargetRef};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CombatDamageAssignmentMode, CombatDamageSubStep, CombatPauseSlot, CombatPreventionTally,
    DamageSlot, GameState, ParkedCombatDamageEntry, PendingCombatDamageBatch,
    PendingCombatLifelink, PendingLifelinkGain, PreventedDamageEntry, WaitingFor,
};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{AppliedReplacementKey, ProposedEvent};
use std::collections::HashMap;

/// CR 510.1a + CR 613.11: Returns the amount of combat damage a creature assigns.
/// Normally equal to power, but if `assigns_damage_from_toughness` is set (e.g. Doran),
/// uses toughness instead. If `assigns_no_combat_damage` is set, returns 0.
fn combat_damage_amount(obj: &GameObject) -> u32 {
    // CR 510.1a: "~ assigns no combat damage" — creature deals 0 combat damage.
    if obj.assigns_no_combat_damage {
        return 0;
    }
    if obj.assigns_damage_from_toughness {
        // CR 613.11 + CR 510.1a: Rule effect uses toughness rather than power.
        obj.toughness.unwrap_or(0).max(0) as u32
    } else {
        // CR 510.1a: Assign combat damage equal to power.
        obj.power.unwrap_or(0).max(0) as u32
    }
}

/// CR 603.2 + CR 704.3: Full trigger/SBA loop after combat damage.
///
/// 1. Collect triggers from damage events while source creatures are still on the battlefield
///    (e.g., DamageReceived for Jackal Pup).
/// 2. Run SBAs (destroy lethal-damage creatures → ZoneChanged events), passing the
///    triggers already collected in this batch so CR 704.5v can see them as
///    "has triggered but not yet left the stack" (a Siege's own victory
///    trigger, still only in `pending`, must not let it die to CR 704.5v).
/// 3. Process triggers from SBA-generated events (e.g., dies triggers from graveyard scan).
/// 4. Repeat SBA/trigger cycle until stable (no new SBAs, no new triggers).
fn process_combat_damage_triggers(
    state: &mut GameState,
    damage_events: &[GameEvent],
    all_events: &mut Vec<GameEvent>,
    include_phase_event: bool,
) {
    // Step 1: Collect triggers from damage events while creatures are still alive.
    // CR 603.2: Triggers fire at the moment the event occurs — process_triggers
    // scans state.battlefield, so this must run before SBAs remove dying objects.
    let mut before_priority_events = Vec::new();
    if include_phase_event {
        before_priority_events.push(GameEvent::PhaseChanged {
            phase: crate::types::phase::Phase::CombatDamage,
        });
    }
    before_priority_events.extend_from_slice(damage_events);
    let mut pending = triggers::collect_triggers_for_batch(state, &before_priority_events);

    // Steps 2-4: SBA/trigger loop per CR 704.3.
    // SBAs may generate events (ZoneChanged for dying creatures) that need trigger
    // processing (dies triggers). Repeat until no new SBAs and no new triggers.
    loop {
        let events_before = all_events.len();
        // CR 704.3 + CR 510.3a + CR 704.5v: SBAs run before this batch is put
        // on the stack, but its abilities have already triggered — a Siege
        // whose CR 310.12b victory trigger waits here is not put into its
        // owner's graveyard.
        sba::check_state_based_actions_with_waiting_triggers(state, all_events, &pending);

        // If SBAs generated new events, process triggers for those events.
        if all_events.len() > events_before {
            let new_events: Vec<_> = all_events[events_before..].to_vec();
            before_priority_events.extend_from_slice(&new_events);
            pending.extend(triggers::collect_triggers_for_batch(state, &new_events));
        } else {
            break;
        }
    }

    if matches!(
        state.waiting_for,
        crate::types::game_state::WaitingFor::GameOver { .. }
    ) {
        return;
    }
    // CR 800.4a: If combat-damage SBAs eliminated a player before this
    // collect-only batch is put on the stack, drop triggers controlled by that
    // player before constructing the APNAP ordering pass.
    pending.retain(|ctx| crate::game::players::is_alive(state, ctx.pending.controller));

    triggers::process_collected_triggers_with_delayed_events(
        state,
        pending,
        &before_priority_events,
        all_events,
    );
}

/// Resolve combat damage with first strike / double strike support (CR 510.1).
/// CR 702.7b: If any creature has first strike or double strike, two damage sub-steps run.
/// Between sub-steps: SBAs are checked and triggers processed.
///
/// Returns `Some(WaitingFor)` when an attacker with 2+ blockers needs interactive
/// damage assignment. Returns `None` when all damage for the current phase is resolved.
/// Re-entrant: call again after the player submits `GameAction::AssignCombatDamage`.
pub fn resolve_combat_damage(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    // CR 510.2 + CR 616.1: a parked PRE-application batch still owns this
    // turn-based action and NONE of its damage may be re-collected (the batch
    // is one event and must never be replayed). Placed AHEAD of
    // `state.combat.as_ref()?` so no combat teardown can strand the record
    // behind that `?`. Damage before lifelink (the single-park invariant
    // makes the order moot; the fixed order aids review).
    if state.pending_combat_damage.is_some() {
        // Any live prompt — the CR 616.1 ordering choice itself, or a
        // sub-choice raised while answering it — means an answer is still
        // coming. Surface it rather than re-collecting assignments.
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return Some(state.waiting_for.clone());
        }
        // Settled to priority with a damage batch still parked. Either the
        // seat that owed the CR 616.1 answer left the game (CR 800.4a:
        // `elimination` cleared `pending_replacement` and the reconcile wrote
        // `Priority { next }`), or a sub-choice of the answer resolved
        // through a dispatcher other than `handle_replacement_choice`. No
        // further `ChooseReplacement` will arrive, so settle the orphaned
        // choice (stashing `None` iff the PAUSED choice died — never for a
        // nested loss) and finish the batch here — otherwise the
        // `!regular_damage_done` completeness gate (priority.rs) re-enters
        // this function forever.
        //
        // `events.len()` folds nothing: no answering action's events are in
        // flight on this path.
        settle_orphaned_combat_damage_choice(state);
        return resume_pending_combat_damage(state, events.len(), events);
    }

    // CR 510.2 + CR 616.1: a parked batch still owns this turn-based action and
    // its damage is ALREADY dealt (the batch is one event and must never be
    // replayed). Placed AHEAD of `state.combat.as_ref()?` so no combat teardown
    // can strand the record behind that `?`.
    if state.pending_combat_lifelink.is_some() {
        // Any live prompt — the CR 616.1 ordering choice itself, or a sub-choice
        // raised while answering it — means an answer is still coming. Surface it
        // rather than re-collecting assignments. The `matches!` below has already
        // excluded `Priority`, so what is returned is always a genuine prompt;
        // `resolve_combat_damage`'s contract never yields a bare `Priority`.
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return Some(state.waiting_for.clone());
        }
        // Settled to priority with a batch still parked. Either the seat that
        // owed the CR 616.1 answer left the game (CR 800.4a: `elimination` cleared
        // `pending_replacement` and the reconcile wrote `Priority { next }`), or a
        // sub-choice of the answer resolved through a dispatcher other than
        // `handle_replacement_choice`. No further `ChooseReplacement` will arrive,
        // so finish the batch here — otherwise the owed gains die with combat AND
        // the `!regular_damage_done` completeness gate (priority.rs) re-enters
        // this function forever.
        //
        // `events.len()` folds nothing: no answering action's events are in
        // flight on this path.
        return resume_pending_combat_lifelink(state, events.len(), events);
    }

    let combat = state.combat.as_ref()?.clone();

    // Guard: regular damage already applied (re-entry from triggers during regular step).
    if combat.regular_damage_done {
        return None;
    }

    let first_strike_participants = combat
        .first_strike_participants
        .clone()
        .unwrap_or_else(|| combat_first_strike_participants(state, &combat));
    if combat.first_strike_participants.is_none() {
        if let Some(current) = state.combat.as_mut() {
            current.first_strike_participants = Some(first_strike_participants.clone());
        }
    }
    let has_first_or_double = !first_strike_participants.is_empty();

    // --- First strike sub-step ---
    if has_first_or_double && !combat.first_strike_done {
        if let Some(waiting) = collect_damage_assignments(state, CombatDamageSubStep::FirstStrike) {
            return Some(waiting);
        }
        // All first-strike assignments collected — apply simultaneously (CR 510.2).
        let pending = take_pending_damage(state);
        let batch = apply_combat_damage(state, &pending, CombatDamageSubStep::FirstStrike);
        let batch_event_start = events.len();
        events.extend_from_slice(batch.events());
        match batch {
            // CR 616.1: the batch parked on an ordering choice — a lifelink
            // life-gain, or (pre-application) a combat-damage event. The
            // sub-step flags are deliberately NOT set here — the resume sets them
            // once the batch actually completes, so neither re-entry nor
            // priority.rs's completeness gate can skip the rest of combat damage.
            CombatDamageBatch::Paused { waiting_for, .. } => {
                claim_combat_lifelink_batch_events_for_ordinary_collection(
                    state,
                    events,
                    batch_event_start,
                );
                return Some(*waiting_for);
            }
            CombatDamageBatch::Complete(damage_events) => {
                if let Some(wf) = finish_combat_damage_sub_step(
                    state,
                    CombatDamageSubStep::FirstStrike,
                    &damage_events,
                    events,
                ) {
                    return Some(wf);
                }
            }
        }
    }

    // --- Regular damage sub-step ---
    // CR 510.4 + CR 702.7b + CR 702.4b: after a first-strike combat damage step
    // the phase gets a second combat damage step, which begins here, just before
    // its damage is assigned (CR 703.4k). A `None` cursor marks the sub-step's
    // first entry; a resume after an assignment prompt leaves it `Some`.
    if state
        .combat
        .as_ref()
        .is_some_and(|c| c.first_strike_done && c.damage_step_index.is_none())
    {
        turns::record_step_begin(state, crate::types::phase::Phase::CombatDamage);
    }
    if let Some(waiting) = collect_damage_assignments(state, CombatDamageSubStep::Regular) {
        return Some(waiting);
    }
    // All regular assignments collected — apply simultaneously (CR 510.2).
    let pending = take_pending_damage(state);
    let batch = apply_combat_damage(state, &pending, CombatDamageSubStep::Regular);
    let batch_event_start = events.len();
    events.extend_from_slice(batch.events());
    match batch {
        // CR 616.1: see the first-strike arm above — the completion flags belong
        // to the resume, not to a batch that has parked.
        CombatDamageBatch::Paused { waiting_for, .. } => {
            claim_combat_lifelink_batch_events_for_ordinary_collection(
                state,
                events,
                batch_event_start,
            );
            Some(*waiting_for)
        }
        CombatDamageBatch::Complete(damage_events) => finish_combat_damage_sub_step(
            state,
            CombatDamageSubStep::Regular,
            &damage_events,
            events,
        ),
    }
}

/// CR 510.2 + CR 510.4 + CR 603.2 + CR 704.3: everything that must happen once
/// ONE simultaneous combat-damage batch is fully applied — sub-step bookkeeping,
/// the trigger/SBA loop, and the sub-step's own completion states.
///
/// Shared by the inline path and the CR 616.1 resume so the two cannot drift.
fn finish_combat_damage_sub_step(
    state: &mut GameState,
    sub_step: CombatDamageSubStep,
    damage_events: &[GameEvent],
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    // CR 500.6: "at the beginning of that step" abilities trigger when the step
    // begins. `process_combat_damage_triggers` synthesizes the PhaseChanged
    // marker for that batch, so the FIRST-STRIKE sub-step always carries it, and
    // the regular sub-step carries it only when it is the ONLY sub-step.
    //
    // Reproduces the previous `!combat.first_strike_done && !has_first_or_double`,
    // which read the entry-time clone. Evaluated BEFORE the `regular_damage_done`
    // write. Equivalent for all three reachable entry states:
    //   * no first strike, single sub-step               -> true  (== old true)
    //   * first-strike sub-step ran earlier in THIS call -> first_strike_done -> false
    //   * re-entry after a prior call's first strike     -> first_strike_done -> false
    let include_phase_event = match sub_step {
        CombatDamageSubStep::FirstStrike => true,
        CombatDamageSubStep::Regular => state.combat.as_ref().is_some_and(|c| {
            !c.first_strike_done
                && c.first_strike_participants
                    .as_ref()
                    .is_none_or(|participants| participants.is_empty())
        }),
    };

    match sub_step {
        CombatDamageSubStep::FirstStrike => {
            if let Some(c) = &mut state.combat {
                c.first_strike_done = true;
                c.damage_step_index = None;
                // CR 510.4: The regular combat-damage sub-step is a fresh assignment.
                // `damage_assignments` is the per-sub-step blocker resume-skip key
                // (CR 702.22k / CR 510.1d), so it MUST be reset between sub-steps —
                // otherwise a double-strike blocker that divided its first-strike
                // damage would be wrongly skipped in the regular sub-step.
                c.damage_assignments.clear();
            }

            // CR 510.4: SBAs and triggers run between first-strike and regular damage sub-steps.
            process_combat_damage_triggers(state, damage_events, events, include_phase_event);

            // CR 510.4 + CR 603.3b: if the first-strike sub-step produced a same-
            // controller trigger-ordering prompt, surface it now — before the regular
            // sub-step's own trigger processing clobbers/orphans it. Returning here
            // leaves `regular_damage_done == false`; the mandatory second (regular)
            // combat-damage sub-step is resumed by the priority-pass completeness gate
            // in priority.rs, which re-enters this function once the order is submitted
            // and the resulting triggers resolve.
            if let Some(waiting_for) = pending_combat_damage_waiting(state) {
                return Some(waiting_for);
            }

            // CR 510.3 + CR 510.3a + CR 510.4: The first-strike combat-damage step is a
            // complete step. Abilities that triggered on first-strike damage (or on SBAs
            // taken afterward) are put on the stack, and THEN the active player receives
            // priority — players must finish with the stack before the second (regular)
            // combat-damage sub-step begins. If the first-strike sub-step placed anything
            // on the stack (e.g. No Mercy's "destroy that creature", a damage trigger that
            // bounces/exiles the attacker), grant priority now so that object resolves
            // first. Skipping this would let a now-doomed double-strike attacker deal its
            // regular-sub-step damage before the trigger that removes it resolves (#692).
            // Returning here leaves `regular_damage_done == false`; the mandatory regular
            // sub-step is resumed once the stack drains and all players pass, via the
            // empty-stack completeness gate in priority.rs.
            if !state.stack.is_empty() {
                // reset_priority here is defensive — unlike the sibling regular-substep entry in
                // turns.rs, this returns mid-step after the first-strike substep, so we explicitly
                // clear any stale passes before the CR 510.3 priority window (harmless if already clear).
                crate::game::priority::reset_priority(state);
                return Some(WaitingFor::Priority {
                    player: state.active_player,
                });
            }
            None
        }
        CombatDamageSubStep::Regular => {
            if let Some(c) = &mut state.combat {
                c.regular_damage_done = true;
                c.damage_step_index = None;
            }

            process_combat_damage_triggers(state, damage_events, events, include_phase_event);
            pending_combat_damage_waiting(state)
        }
    }
}

/// CR 616.1 + CR 510.2 + CR 702.15b: finish a combat-damage batch that parked on
/// a lifelink life-gain ordering choice.
///
/// `action_event_start` is the index at which the ANSWERING action's events
/// begin. It is a parameter, never stored: `handle_replacement_choice` captures
/// it as a local before the answer is applied, and the re-entry guard passes
/// `events.len()` when no answer is in flight. Both are `<= events.len()` by
/// construction, so the slice below cannot be out of range.
///
/// The answering action's events — the resumed `LifeChanged` among them — are
/// folded into the parked batch, so "whenever you gain life" observers
/// (CR 119.9) join the SAME CR 603.3b batch as the combat-damage observers.
/// CR 702.15b makes the gain simultaneous with the damage, and no player
/// received priority for the CR 616.1 choice, so splitting them would be a rules
/// regression.
///
/// Returns `None` when nothing is parked, so callers can keep their own wait.
pub(crate) fn resume_pending_combat_lifelink(
    state: &mut GameState,
    action_event_start: usize,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let mut record = *state.pending_combat_lifelink.take()?;
    record
        .batch_events
        .extend_from_slice(&events[action_event_start..]);
    let owed_from = record.batch_events.len();
    let sub_step = record.sub_step;
    match drain_combat_lifelink(state, record) {
        CombatDamageBatch::Paused {
            events: batch,
            waiting_for,
        } => {
            // CR 616.1f: a LATER source needed its own ordering choice. The
            // still-untouched tail was re-parked with the batch so far, so the
            // eventual completion still fires ONE CR 603.3b batch.
            events.extend_from_slice(&batch[owed_from..]);
            claim_combat_lifelink_batch_events_for_ordinary_collection(
                state,
                events,
                action_event_start,
            );
            Some(*waiting_for)
        }
        CombatDamageBatch::Complete(damage_events) => {
            events.extend_from_slice(&damage_events[owed_from..]);
            let waiting_for =
                finish_combat_damage_sub_step(state, sub_step, &damage_events, events);
            claim_combat_lifelink_batch_events_for_ordinary_collection(
                state,
                events,
                action_event_start,
            );
            if let Some(wf) = waiting_for {
                return Some(wf);
            }
            // CR 510.4: the second combat damage step is MANDATORY. Re-enter so a
            // paused first-strike batch still runs it, exactly as the inline path
            // does. Bounded: the record was `.take()`n above, so the re-entry
            // guard cannot route back here; a regular sub-step that parks returns
            // its own prompt without recursing, and a regular sub-step already
            // done short-circuits on `regular_damage_done`.
            resolve_combat_damage(state, events).or(Some(WaitingFor::Priority {
                player: state.active_player,
            }))
        }
    }
}

/// CR 616.1 + CR 800.4a: settle a paused combat-damage batch whose choice
/// authority died with its seat (`elimination` cleared `pending_replacement`).
/// Shared by the resolve guard and the turn discharge so neither can hang.
///
/// The stash-state len gate doubles as the which-choice-died discriminator:
/// iff UNSTASHED (`decided.len() == paused_entry`) the PAUSED choice died —
/// stash `None` at the paused slot (taking `paused_entry_pruned` when set:
/// damage to departed recipients does nothing, CR 800.4e/614.9), symmetric to
/// the lifelink "never re-queued" loss. Iff STASHED
/// (`len == paused_entry + 1`, the nested window) the died choice was NESTED,
/// not the paused event — drop it with NO stash (the paused slot's stash is
/// intact) and let the resume proceed. A live authority (`pending_replacement`
/// still `Some`) settles nothing: the paused event is still owned by it.
pub(crate) fn settle_orphaned_combat_damage_choice(state: &mut GameState) {
    if state.pending_replacement.is_some() {
        return;
    }
    let Some(record) = state.pending_combat_damage.as_mut() else {
        return;
    };
    if record.decided.len() == record.paused_entry {
        record.decided.push(None);
        record.paused_entry_pruned = false;
    }
}

/// CR 510.2 + CR 616.1: finish a combat-damage batch paused on a replacement
/// ordering choice. Runs the unprocessed remainder through the batch pipeline
/// (re-pausing on a further choice), merges every leg's survivors with the
/// parked decided prefix, and completes through the SAME shared tail as the
/// inline path (`apply_combat_survivors`) so the two cannot drift — the batch
/// is one simultaneity even across the pause.
///
/// `action_event_start` is the caller's boundary: the answering action's
/// events are folded into the parked batch (see
/// `resume_pending_combat_lifelink`), so observers see ONE CR 603.3b batch.
///
/// CR 510.2 + CR 616.1 (Matt R12): bind the paused batch member to its
/// replacement continuation with stable ownership captured at the pause point.
/// Stamps `state.pending_replacement` with the paused slot (`paused_entry` +
/// the member's damage source) so the answer-stash gate survives recipient
/// rewrites between choices. Stamps ONLY the member's own event park: the
/// parked `proposed` must be a Damage event from the member's source (a
/// redirect may already have rewritten its recipient — that is exactly the
/// case the stamp exists for). A nested child's park names a different source
/// and stays unstamped, so its answer still delivers inline exactly once.
/// Both park points (first park and re-pause) stamp; a re-parked prompt
/// inherits via the NeedsChoice epilogue instead.
fn stamp_combat_pause_slot(state: &mut GameState, paused_entry: usize, source: ObjectId) {
    let own_park = state.pending_replacement.as_ref().is_some_and(|pending| {
        matches!(&pending.proposed, ProposedEvent::Damage { source_id, .. } if *source_id == source)
    });
    if !own_park {
        return;
    }
    if let Some(pending) = state.pending_replacement.as_mut() {
        pending.combat_pause_slot = Some(CombatPauseSlot {
            paused_entry,
            source,
        });
    }
}

/// Returns `None` when nothing is parked, so callers can keep their own wait.
pub(crate) fn resume_pending_combat_damage(
    state: &mut GameState,
    action_event_start: usize,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let mut record = state.pending_combat_damage.take()?;
    debug_assert!(
        state.pending_combat_lifelink.is_none(),
        "single-park invariant: the damage record is taken before the lifelink drain runs"
    );
    record
        .batch_events
        .extend_from_slice(&events[action_event_start..]);
    let owed_from = record.batch_events.len();
    // The answer stashed the paused slot (or the guard/discharge settled it):
    // `decided` now covers `entries[..=paused_entry]`.
    debug_assert_eq!(
        record.decided.len(),
        record.paused_entry + 1,
        "a resumed batch must carry its paused slot's stash"
    );
    // Install the parked prefix as the live window so the remaining run
    // accumulates onto it. `outer_tally` (the pre-batch window) is restored
    // by the shared tail on completion, or now on re-pause. The live window
    // is `#[serde(skip)]`, so a mid-pause save/load drops it: prefer it when
    // present, else hydrate the pre-batch window the park captured.
    let outer_tally = state.combat_prevention_tally.take().or_else(|| {
        record
            .outer_tally
            .as_ref()
            .map(|entries| tally_map_from_record_entries(entries))
    });
    let mut live = tally_map_from_record_entries(&record.phase_a_tally);
    for (key, amount) in tally_map_from_record_entries(&record.phase_b_tally) {
        *live.entry(key).or_insert(0) += amount;
    }
    state.combat_prevention_tally = Some(live);
    let sub_step = record.sub_step;
    match replacement::replace_combat_damage_batch(
        state,
        &mut record.batch_events,
        std::mem::take(&mut record.remaining),
    ) {
        replacement::CombatDamageReplacementOutcome::Complete {
            survivors: rest,
            tally,
        } => {
            if let Some(live) = state.combat_prevention_tally.as_mut() {
                for (key, amount) in tally {
                    *live.entry(key).or_insert(0) += amount;
                }
            }
            let combined = state.combat_prevention_tally.take().unwrap_or_default();
            let mut survivors = std::mem::take(&mut record.decided);
            survivors.extend(rest);
            let hydrated: Vec<HydratedCombatEntry> = record
                .entries
                .into_iter()
                .map(|entry| HydratedCombatEntry {
                    ctx: DamageContext::from(entry.ctx),
                    source_is_commander: entry.source_is_commander,
                    assignment: entry.assignment,
                    lifelink_controller: entry.lifelink_controller,
                })
                .collect();
            let batch_events = std::mem::take(&mut record.batch_events);
            // The combined tally rides as the Phase-A leg (merging is keyed
            // sums, so one leg + empty equals the inline two-leg merge).
            match apply_combat_survivors(
                state,
                hydrated,
                survivors,
                combined,
                HashMap::new(),
                outer_tally,
                std::mem::take(&mut record.lives_before),
                sub_step,
                batch_events,
            ) {
                CombatDamageBatch::Paused {
                    events: batch,
                    waiting_for,
                } => {
                    // CR 616.1f: a lifelink gain paused inside the resumed
                    // batch. The still-untouched tail was re-parked with the
                    // batch so far, so the eventual completion still fires ONE
                    // CR 603.3b batch.
                    events.extend_from_slice(&batch[owed_from..]);
                    claim_combat_lifelink_batch_events_for_ordinary_collection(
                        state,
                        events,
                        action_event_start,
                    );
                    Some(*waiting_for)
                }
                CombatDamageBatch::Complete(damage_events) => {
                    events.extend_from_slice(&damage_events[owed_from..]);
                    let waiting_for =
                        finish_combat_damage_sub_step(state, sub_step, &damage_events, events);
                    claim_combat_lifelink_batch_events_for_ordinary_collection(
                        state,
                        events,
                        action_event_start,
                    );
                    if let Some(wf) = waiting_for {
                        return Some(wf);
                    }
                    // CR 510.4: the second combat damage step is MANDATORY.
                    // Re-enter so a paused first-strike batch still runs it,
                    // exactly as the inline path does. Bounded: the record was
                    // `.take()`n above, so the re-entry guard cannot route back
                    // here; a regular sub-step that parks returns its own
                    // prompt without recursing, and a regular sub-step already
                    // done short-circuits on `regular_damage_done`.
                    resolve_combat_damage(state, events).or(Some(WaitingFor::Priority {
                        player: state.active_player,
                    }))
                }
            }
        }
        replacement::CombatDamageReplacementOutcome::Paused {
            decided: rest_decided,
            remaining: rest_remaining,
            tally,
            player,
        } => {
            // CR 616.1f: a LATER event needed its own ordering choice.
            // Snapshot the live window back into ONE field (clearing Phase A
            // so reinstall is unambiguous), restore the outer window, and park
            // a FRESH record — never mutate the old one, so no stale
            // `paused_entry_pruned` flag can survive across parks. The fresh
            // park re-establishes `decided.len() == paused_entry`, re-arming
            // the answer-stash gate by construction.
            if let Some(live) = state.combat_prevention_tally.as_mut() {
                for (key, amount) in tally {
                    *live.entry(key).or_insert(0) += amount;
                }
            }
            let combined = state.combat_prevention_tally.take().unwrap_or_default();
            let outer_entries = outer_tally.as_ref().map(tally_entries_for_record);
            state.combat_prevention_tally = outer_tally;
            let mut decided = std::mem::take(&mut record.decided);
            decided.extend(rest_decided);
            let paused_entry = decided.len();
            let fresh = PendingCombatDamageBatch {
                entries: std::mem::take(&mut record.entries),
                decided,
                paused_entry,
                paused_entry_pruned: false,
                remaining: rest_remaining,
                phase_a_tally: Vec::new(),
                phase_b_tally: tally_entries_for_record(&combined),
                outer_tally: outer_entries,
                batch_events: std::mem::take(&mut record.batch_events),
                lives_before: std::mem::take(&mut record.lives_before),
                sub_step: record.sub_step,
            };
            // CR 510.2 + CR 616.1 (Matt R12): re-bind the later paused member
            // to its fresh prompt — the re-pause re-arms the stash gate for a
            // NEW slot, so it needs a NEW stamp (the old prompt was consumed
            // with its answer).
            if let Some(entry) = fresh.entries.get(paused_entry) {
                stamp_combat_pause_slot(state, paused_entry, entry.ctx.source_id);
            }
            // CR 732.2a: the batch may have moved life (an inline redirect or
            // rider leg). Invalidate now as well as at completion, against the
            // SAME pre-batch snapshot. The guard only ever CLEARS, so the
            // paired calls are idempotent.
            state.invalidate_loop_ring_on_unobserved_life_move(&fresh.lives_before);
            let waiting_for = replacement::replacement_choice_waiting_for(player, state);
            state.waiting_for = waiting_for.clone();
            let returned = fresh.batch_events.clone();
            state.pending_combat_damage = Some(Box::new(fresh));
            events.extend_from_slice(&returned[owed_from..]);
            claim_combat_lifelink_batch_events_for_ordinary_collection(
                state,
                events,
                action_event_start,
            );
            Some(waiting_for)
        }
    }
}

/// CR 510.2 + CR 603.2c: a combat-damage batch parked for a CR 616.1 choice
/// still owns its ordinary trigger collection. Suppress only the generic
/// post-action ordinary collector for the exact full-buffer occurrences; the
/// resumed batch collector receives the retained events, and delayed collectors
/// retain their CR 603.7b visibility.
pub(crate) fn claim_combat_lifelink_batch_events_for_ordinary_collection(
    state: &mut GameState,
    events: &[GameEvent],
    event_start: usize,
) {
    triggers::claim_events_for_ordinary_collection(state, events, event_start);
}

/// Returns a terminal or trigger-ordering state produced while combat damage resolves.
///
/// CR 603.3b / CR 704.3: trigger ordering and game-over results are completion
/// states of the combat-damage resolver, so callers must receive them rather than
/// replacing them with a fresh priority window.
fn pending_combat_damage_waiting(state: &GameState) -> Option<WaitingFor> {
    match &state.waiting_for {
        WaitingFor::OrderTriggers { .. } | WaitingFor::GameOver { .. } => {
            Some(state.waiting_for.clone())
        }
        _ => None,
    }
}

fn combat_first_strike_participants(
    state: &GameState,
    combat: &CombatState,
) -> std::collections::HashSet<ObjectId> {
    // CR 510.4: the trigger is "at least one attacking or blocking creature"
    // with first or double strike; a blocking creature whose attackers all left
    // combat is still a blocking creature (CR 509.1g), so every key counts.
    combat
        .attackers
        .iter()
        .map(|attacker| attacker.object_id)
        .chain(combat.blocker_to_attacker.keys().copied())
        .filter(|object_id| {
            state.objects.get(object_id).is_some_and(|object| {
                object.has_keyword(&Keyword::FirstStrike)
                    || object.has_keyword(&Keyword::DoubleStrike)
            })
        })
        .collect()
}

fn deals_in_substep(
    obj: &GameObject,
    sub_step: CombatDamageSubStep,
    first_strike_participants: &std::collections::HashSet<ObjectId>,
) -> bool {
    match sub_step {
        CombatDamageSubStep::FirstStrike => first_strike_participants.contains(&obj.id),
        CombatDamageSubStep::Regular => {
            !first_strike_participants.contains(&obj.id) || obj.has_keyword(&Keyword::DoubleStrike)
        }
    }
}

/// Whether a combatant will assign nonzero damage in the pending combat-damage
/// substep under the engine's current first/double-strike state.
///
/// CR 510.4 + CR 702.7b: after the first-strike substep, first-strike-only
/// creatures do not participate in the regular substep, while double-strike
/// creatures do. This is the shared query for consumers that must reason about
/// the next damage event without duplicating combat-substep selection.
///
/// CR 510.1d: a blocking creature none of whose attackers remain in combat is
/// still a blocking creature but assigns no combat damage, so it does not
/// participate.
pub fn participates_in_pending_combat_damage_substep(
    state: &GameState,
    object_id: ObjectId,
) -> bool {
    let Some(combat) = state.combat.as_ref() else {
        return false;
    };
    if combat.regular_damage_done
        || (!combat
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == object_id)
            && !combat
                .blocker_to_attacker
                .get(&object_id)
                .is_some_and(|attackers| !attackers.is_empty()))
    {
        return false;
    }
    let Some(object) = state
        .objects
        .get(&object_id)
        .filter(|object| object.zone == crate::types::zones::Zone::Battlefield)
    else {
        return false;
    };
    let first_strike_participants = combat
        .first_strike_participants
        .clone()
        .unwrap_or_else(|| combat_first_strike_participants(state, combat));
    let sub_step = if !first_strike_participants.is_empty() && !combat.first_strike_done {
        CombatDamageSubStep::FirstStrike
    } else {
        CombatDamageSubStep::Regular
    };
    deals_in_substep(object, sub_step, &first_strike_participants)
        && combat_damage_amount(object) > 0
}

/// Damage amounts fixed by an ordinary one-on-one combat exchange.
///
/// This deliberately describes only damage marked on each combatant; it does
/// not imply that either combatant survives state-based actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedCombatDamage {
    pub attacker_to_blocker: u32,
    pub blocker_to_attacker: u32,
}

/// Survival facts derived from a [`FixedCombatDamage`] exchange.
///
/// This is separate from the damage fact because damage can be fixed while
/// survival is not once destruction replacements or other post-damage effects
/// are relevant. `assess_combat_impact` reports this only when both are fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedCombatSurvival {
    pub attacker_survives: bool,
    pub blocker_survives: bool,
}

/// Conservative assessment of a bound attacker/blocker pair's next combat-damage exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombatImpact {
    Fixed {
        damage: FixedCombatDamage,
        survival: FixedCombatSurvival,
    },
    Indeterminate,
}

#[allow(dead_code)] // Reachable only through the deferred crate-visible assessment API.
fn combatant_has_regeneration_shield(object: &GameObject) -> bool {
    object
        .replacement_definitions
        .iter_all()
        .any(|replacement| replacement.shield_kind == ShieldKind::Regeneration)
}

#[allow(dead_code)] // Reachable only through the deferred crate-visible assessment API.
fn damage_is_not_fixed(
    state: &GameState,
    context: &DamageContext,
    target: &TargetRef,
    amount: u32,
) -> bool {
    if amount == 0 {
        return false;
    }

    let DamageGateOutcome::Proposed(proposed) =
        pre_replacement_damage_gate(state, context, target, amount, true)
    else {
        return true;
    };

    !replacement::find_applicable_replacements(
        state,
        &proposed,
        replacement::replacement_registry(),
    )
    .is_empty()
}

#[allow(dead_code)] // Reachable only through the deferred crate-visible assessment API.
fn lethal_destruction_is_not_fixed(state: &GameState, object_id: ObjectId) -> bool {
    let proposed = ProposedEvent::Destroy {
        object_id,
        source: None,
        cant_regenerate: false,
        applied: Default::default(),
    };
    !replacement::find_applicable_replacements(
        state,
        &proposed,
        replacement::replacement_registry(),
    )
    .is_empty()
}

#[allow(dead_code)] // Reachable only through the deferred crate-visible assessment API.
fn survives_marked_damage(object: &GameObject, incoming_damage: u32) -> Option<bool> {
    let toughness = object.toughness?.max(0) as u32;
    Some(object.damage_marked.saturating_add(incoming_damage) < toughness)
}

/// Assess the next combat-damage exchange for a currently bound attacker/blocker pair.
///
/// This deliberately fails closed: it only returns fixed facts for an ordinary,
/// one-on-one, simultaneous exchange with no applicable damage replacement or
/// pre-replacement prevention gate. It never runs the replacement pipeline,
/// applies damage, clones state, or rebuilds the replacement index.
///
/// CR 510.2: Combat damage is dealt simultaneously. CR 614.1 + CR 615.1:
/// replacement and prevention effects must be resolved before that damage is dealt.
pub(crate) fn assess_combat_impact(
    state: &GameState,
    attacker: ObjectIncarnationRef,
    blocker: ObjectIncarnationRef,
) -> CombatImpact {
    let Some(combat) = state.combat.as_ref() else {
        return CombatImpact::Indeterminate;
    };
    if combat.first_strike_done
        || combat.regular_damage_done
        || combat.damage_step_index.is_some()
        || !combat.pending_damage.is_empty()
        || !combat.damage_assignments.is_empty()
        || matches!(
            &state.waiting_for,
            WaitingFor::AssignCombatDamage { .. } | WaitingFor::AssignBlockerDamage { .. }
        )
    {
        return CombatImpact::Indeterminate;
    }

    let Some(attacker_object) = state.objects.get(&attacker.object_id) else {
        return CombatImpact::Indeterminate;
    };
    let Some(blocker_object) = state.objects.get(&blocker.object_id) else {
        return CombatImpact::Indeterminate;
    };
    if ObjectIncarnationRef::from_object(attacker_object) != attacker
        || ObjectIncarnationRef::from_object(blocker_object) != blocker
        || attacker.object_id == blocker.object_id
        || attacker_object.zone != crate::types::zones::Zone::Battlefield
        || blocker_object.zone != crate::types::zones::Zone::Battlefield
        || !attacker_object
            .card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Creature)
        || !blocker_object
            .card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Creature)
    {
        return CombatImpact::Indeterminate;
    }

    let Some(attacker_info) = combat
        .attackers
        .iter()
        .find(|entry| entry.object_id == attacker.object_id)
    else {
        return CombatImpact::Indeterminate;
    };
    if !attacker_info.blocked
        || attacker_info.band_id.is_some()
        || combat
            .blocker_assignments
            .get(&attacker.object_id)
            .is_none_or(|blockers| blockers.as_slice() != [blocker.object_id])
        || combat
            .blocker_to_attacker
            .get(&blocker.object_id)
            .is_none_or(|attackers| attackers.as_slice() != [attacker.object_id])
        || combat
            .first_strike_participants
            .as_ref()
            .is_some_and(|participants| !participants.is_empty())
        || !combat_first_strike_participants(state, combat).is_empty()
    {
        return CombatImpact::Indeterminate;
    }

    let attacker_context = DamageContext::from_source(state, attacker.object_id);
    let blocker_context = DamageContext::from_source(state, blocker.object_id);
    let (Some(attacker_context), Some(blocker_context)) = (attacker_context, blocker_context)
    else {
        return CombatImpact::Indeterminate;
    };
    if attacker_context.has_deathtouch
        || blocker_context.has_deathtouch
        || attacker_context.has_wither
        || blocker_context.has_wither
        || attacker_context.has_infect
        || blocker_context.has_infect
        || attacker_object.assigns_damage_as_though_unblocked
        || blocker_object.assigns_damage_as_though_unblocked
        || crate::game::combat::has_banding(state, attacker.object_id)
        || crate::game::combat::has_banding(state, blocker.object_id)
        || attacker_object.has_keyword(&Keyword::Trample)
        || attacker_object.has_keyword(&Keyword::TrampleOverPlaneswalkers)
        || blocker_object.has_keyword(&Keyword::Trample)
        || blocker_object.has_keyword(&Keyword::TrampleOverPlaneswalkers)
        || attacker_object.has_keyword(&Keyword::Indestructible)
        || blocker_object.has_keyword(&Keyword::Indestructible)
        || attacker_object.counters.contains_key(&CounterType::Shield)
        || blocker_object.counters.contains_key(&CounterType::Shield)
        || combatant_has_regeneration_shield(attacker_object)
        || combatant_has_regeneration_shield(blocker_object)
    {
        return CombatImpact::Indeterminate;
    }

    let damage = FixedCombatDamage {
        attacker_to_blocker: combat_damage_amount(attacker_object),
        blocker_to_attacker: combat_damage_amount(blocker_object),
    };
    if damage_is_not_fixed(
        state,
        &attacker_context,
        &TargetRef::Object(blocker.object_id),
        damage.attacker_to_blocker,
    ) || damage_is_not_fixed(
        state,
        &blocker_context,
        &TargetRef::Object(attacker.object_id),
        damage.blocker_to_attacker,
    ) {
        return CombatImpact::Indeterminate;
    }

    let (Some(attacker_survives), Some(blocker_survives)) = (
        survives_marked_damage(attacker_object, damage.blocker_to_attacker),
        survives_marked_damage(blocker_object, damage.attacker_to_blocker),
    ) else {
        return CombatImpact::Indeterminate;
    };
    if (!attacker_survives && lethal_destruction_is_not_fixed(state, attacker.object_id))
        || (!blocker_survives && lethal_destruction_is_not_fixed(state, blocker.object_id))
    {
        return CombatImpact::Indeterminate;
    }

    CombatImpact::Fixed {
        damage,
        survival: FixedCombatSurvival {
            attacker_survives,
            blocker_survives,
        },
    }
}

/// Drain pending_damage from CombatState, resetting it to empty.
fn take_pending_damage(state: &mut GameState) -> Vec<(ObjectId, DamageAssignment)> {
    state
        .combat
        .as_mut()
        .map(|c| std::mem::take(&mut c.pending_damage))
        .unwrap_or_default()
}

/// Iterate attackers (and blockers) for a sub-step, collecting auto-assigned damage
/// into `combat.pending_damage`. Returns `Some(WaitingFor::AssignCombatDamage)` when
/// an attacker has 2+ blockers and needs interactive assignment.
fn collect_damage_assignments(
    state: &mut GameState,
    sub_step: CombatDamageSubStep,
) -> Option<WaitingFor> {
    let combat = state.combat.as_ref()?.clone();
    let start_index = combat.damage_step_index.unwrap_or(0);
    let first_strike_participants = combat
        .first_strike_participants
        .as_ref()
        .expect("combat damage participant snapshot is initialized before assignment");

    // --- Attackers ---
    for (i, attacker_info) in combat.attackers.iter().enumerate().skip(start_index) {
        let obj = match state.objects.get(&attacker_info.object_id) {
            Some(o) if o.zone == crate::types::zones::Zone::Battlefield => o,
            _ => continue,
        };

        if !deals_in_substep(obj, sub_step, first_strike_participants) {
            continue;
        }

        let power = combat_damage_amount(obj);
        if power == 0 {
            continue;
        }

        let has_deathtouch = obj.has_keyword(&Keyword::Deathtouch);
        // CR 702.19c takes precedence when both present — it subsumes regular trample behavior
        let trample = if obj.has_keyword(&Keyword::TrampleOverPlaneswalkers) {
            Some(TrampleKind::OverPlaneswalkers)
        } else if obj.has_keyword(&Keyword::Trample) {
            Some(TrampleKind::Standard)
        } else {
            None
        };

        // CR 510.1c + CR 702.19b: Check if interactive assignment is needed
        // (2+ blockers, or a single-blocker trample attacker with excess damage).
        if needs_interactive_assignment(
            state,
            obj,
            &combat,
            attacker_info,
            power,
            has_deathtouch,
            trample,
        ) {
            // Pause iteration — player must choose damage division.
            if let Some(c) = &mut state.combat {
                c.damage_step_index = Some(i);
            }

            let blocker_ids = combat
                .blocker_assignments
                .get(&attacker_info.object_id)
                .cloned()
                .unwrap_or_default();
            let blockers: Vec<DamageSlot> = combat
                .blocker_assignments
                .get(&attacker_info.object_id)
                .into_iter()
                .flatten()
                .map(|&bid| DamageSlot {
                    blocker_id: bid,
                    lethal_minimum: lethal_damage_needed(state, bid, has_deathtouch),
                })
                .collect();
            let assignment_modes = combat_damage_assignment_modes(
                obj,
                attacker_info.blocked,
                !blocker_ids.is_empty(),
                trample,
            );

            // The player who assigns damage is normally the attacker's controller.
            let mut controller = state
                .objects
                .get(&attacker_info.object_id)
                .map(|o| o.controller)
                .unwrap_or(state.active_player);

            // CR 702.22j: During the combat damage step, if an attacking creature
            // is being blocked by a creature with banding or a qualifying "bands
            // with other" relation, the DEFENDING player (rather than the active
            // player) chooses how the attacking creature's damage is assigned.
            // We reach this branch whenever an interactive assignment is required
            // — both the multi-blocker case and the single banded-blocker-with-
            // trample case.
            let blocked_by_banding = combat
                .blocker_assignments
                .get(&attacker_info.object_id)
                .into_iter()
                .flatten()
                .any(|&bid| crate::game::combat::has_banding(state, bid))
                || crate::game::combat::has_bands_with_other_damage_assignment_group(
                    state,
                    &blocker_ids,
                );
            if blocked_by_banding {
                controller = attacker_info.defending_player;
            }

            // CR 702.19c: Compute PW loyalty threshold for trample-over-PW spillover
            let (pw_loyalty, pw_controller) = if trample == Some(TrampleKind::OverPlaneswalkers) {
                compute_pw_loyalty_threshold(state, &attacker_info.attack_target)
            } else {
                (None, None)
            };

            return Some(WaitingFor::AssignCombatDamage {
                player: controller,
                attacker_id: attacker_info.object_id,
                total_damage: power,
                blockers,
                assignment_modes,
                trample,
                defending_player: attacker_info.defending_player,
                attack_target: attacker_info.attack_target,
                pw_loyalty,
                pw_controller,
            });
        }

        // Auto-assign for unblocked, single blocker, or blocked-but-no-current-blockers.
        let assignments = assign_attacker_damage(
            state,
            attacker_info,
            &combat,
            power,
            has_deathtouch,
            trample,
        );
        if let Some(c) = &mut state.combat {
            for a in assignments {
                c.pending_damage.push((attacker_info.object_id, a));
            }
        }
    }

    // CR 510.1c/d: The attacker loop has fully processed every attacker for this
    // sub-step. Advance the cursor past the end so that a later re-entry (e.g.
    // after an interactive CR 702.22k blocker-division prompt below) skips the
    // attacker loop and does NOT re-push attacker auto-assignments — which would
    // double-count attacker damage. Without an interactive blocker this is a
    // no-op (the function returns `None` at the bottom and the sub-step applies).
    if let Some(c) = &mut state.combat {
        c.damage_step_index = Some(c.attackers.len());
    }

    // --- Blockers ---
    // CR 510.1d: A blocking creature assigns its combat damage to the creatures
    // it's blocking, divided as its controller chooses (CR 702.22k re-routes the
    // chooser to the active player when a blocked attacker has banding).
    //
    // Re-entrancy: unlike attackers (a `Vec` indexed by `damage_step_index`),
    // blockers live in a `HashMap` with no positional cursor. The resume-skip key
    // is membership in `combat.damage_assignments` (keyed by blocker id), which is
    // recorded for EVERY processed blocker — both the auto-even-split path and the
    // interactive CR 702.22k path. This keeps an already-divided blocker from being
    // re-pushed when we re-enter after an interactive prompt resolves. The
    // skip-key set is reset per sub-step (see `resolve_combat_damage`), so a
    // double-strike blocker is correctly re-processed in the regular sub-step.
    //
    // Deterministic order: `ObjectId(pub u64)` is not `Ord`, so we sort the
    // blocker ids by their inner `u64` to guarantee a stable prompt sequence
    // across AI clones and save/reload.
    let mut blocker_ids: Vec<ObjectId> = combat.blocker_to_attacker.keys().copied().collect();
    blocker_ids.sort_by_key(|id| id.0);

    for blocker_id in blocker_ids {
        // Resume-skip: this blocker's division was already recorded this sub-step.
        if combat
            .damage_assignments
            .get(&blocker_id)
            .is_some_and(|v| !v.is_empty())
        {
            continue;
        }
        let attacker_ids = match combat.blocker_to_attacker.get(&blocker_id) {
            Some(ids) => ids,
            None => continue,
        };
        // CR 510.1d: a blocking creature that isn't currently blocking any
        // creature (its attackers were removed from combat, CR 506.4) assigns no
        // combat damage. No resume-skip key is recorded; re-entry skips it here.
        if attacker_ids.is_empty() {
            continue;
        }
        let obj = match state.objects.get(&blocker_id) {
            Some(o) if o.zone == crate::types::zones::Zone::Battlefield => o,
            _ => continue,
        };

        if !deals_in_substep(obj, sub_step, first_strike_participants) {
            continue;
        }

        let power = combat_damage_amount(obj);
        if power == 0 {
            continue;
        }

        // CR 702.22k: During the combat damage step, if a blocking creature is
        // blocking a creature with banding or a qualifying "bands with other"
        // relation, the ACTIVE player (rather than the defending player) chooses
        // how the blocking creature's damage is divided among the attackers it's
        // blocking. This only matters when the blocker is blocking 2+ attackers.
        let active_player_divides = attacker_ids.len() >= 2
            && (attacker_ids
                .iter()
                .any(|&aid| crate::game::combat::has_banding(state, aid))
                || crate::game::combat::has_bands_with_other_damage_assignment_group(
                    state,
                    attacker_ids,
                ));

        if active_player_divides {
            return Some(WaitingFor::AssignBlockerDamage {
                player: state.active_player,
                blocker_id,
                total_damage: power,
                attackers: attacker_ids.clone(),
            });
        }

        // CR 510.1d: default — controller's blocker divides evenly (auto-split).
        let blocker_assignments = distribute_blocker_damage(blocker_id, power, attacker_ids);
        if let Some(c) = &mut state.combat {
            c.pending_damage.extend(blocker_assignments);
            // Record into the resume-skip key so re-entry after an interactive
            // CR 702.22k prompt does not re-push this blocker's division.
            c.damage_assignments.insert(
                blocker_id,
                vec![DamageAssignment {
                    target: DamageTarget::Object(attacker_ids[0]),
                    amount: power,
                }],
            );
        }
    }

    // All done for this sub-step — reset index.
    if let Some(c) = &mut state.combat {
        c.damage_step_index = None;
    }
    None
}

/// CR 510.1d: Distribute a blocker's combat damage among the attackers it blocks.
/// When blocking multiple attackers, damage is split evenly (first attacker gets remainder).
fn distribute_blocker_damage(
    blocker_id: ObjectId,
    power: u32,
    attacker_ids: &[ObjectId],
) -> Vec<(ObjectId, DamageAssignment)> {
    if attacker_ids.is_empty() {
        return Vec::new();
    }
    if attacker_ids.len() == 1 {
        return vec![(
            blocker_id,
            DamageAssignment {
                target: DamageTarget::Object(attacker_ids[0]),
                amount: power,
            },
        )];
    }
    // Split damage evenly; first attacker gets the remainder
    let n = attacker_ids.len() as u32;
    let base = power / n;
    let remainder = power % n;
    attacker_ids
        .iter()
        .enumerate()
        .filter_map(|(i, &aid)| {
            let amount = base + if (i as u32) < remainder { 1 } else { 0 };
            if amount == 0 {
                None
            } else {
                Some((
                    blocker_id,
                    DamageAssignment {
                        target: DamageTarget::Object(aid),
                        amount,
                    },
                ))
            }
        })
        .collect()
}

/// CR 510.1c: Check if an attacker needs interactive damage assignment.
/// Returns true when there are 2+ blockers — the attacking player should choose
/// how to divide damage. Single-blocker and unblocked scenarios are auto-assigned,
/// except a single-blocker trample attacker with excess damage (see CR 702.19b below).
pub(crate) fn needs_interactive_assignment(
    state: &GameState,
    obj: &GameObject,
    combat: &CombatState,
    attacker_info: &crate::game::combat::AttackerInfo,
    power: u32,
    has_deathtouch: bool,
    trample: Option<TrampleKind>,
) -> bool {
    let blocker_count = combat
        .blocker_assignments
        .get(&attacker_info.object_id)
        .map_or(0, Vec::len);

    if obj.assigns_damage_as_though_unblocked && attacker_info.blocked {
        let has_trample = obj.has_keyword(&Keyword::Trample)
            || obj.has_keyword(&Keyword::TrampleOverPlaneswalkers);
        return blocker_count > 0 || !has_trample;
    }

    // CR 702.19b: A trample attacker blocked by exactly one creature still gets an
    // interactive choice when it has excess damage — the controller may keep the
    // excess on the blocker or assign it to the player (assigning none to the player
    // is legal). With no excess (power <= lethal) all damage goes to the blocker
    // (CR 510.1c), so there is no choice → auto-assign.
    if trample.is_some() && blocker_count == 1 {
        if let Some(&blocker_id) = combat
            .blocker_assignments
            .get(&attacker_info.object_id)
            .and_then(|b| b.first())
        {
            return power > lethal_damage_needed(state, blocker_id, has_deathtouch);
        }
    }

    blocker_count >= 2
}

/// CR 702.19c: Compute effective PW loyalty threshold for trample-over-PW,
/// accounting for pending damage from other attackers in the same step.
fn compute_pw_loyalty_threshold(
    state: &GameState,
    attack_target: &AttackTarget,
) -> (Option<u32>, Option<PlayerId>) {
    if let AttackTarget::Planeswalker(pw_id) = attack_target {
        // CR 306.8: PW loyalty is tracked via the `loyalty` field (authoritative),
        // synced with counters on damage application. Read the field directly.
        let base_loyalty = state
            .objects
            .get(pw_id)
            .and_then(|obj| obj.loyalty)
            .unwrap_or(0);
        // CR 702.19c: Account for pending damage from other attackers this step
        let pending_to_pw: u32 = state
            .combat
            .as_ref()
            .map(|c| {
                c.pending_damage
                    .iter()
                    .filter(|(_, da)| da.target == DamageTarget::Object(*pw_id))
                    .map(|(_, da)| da.amount)
                    .sum()
            })
            .unwrap_or(0);
        let effective = base_loyalty.saturating_sub(pending_to_pw);
        let controller = state.objects.get(pw_id).map(|obj| obj.controller);
        (Some(effective), controller)
    } else {
        (None, None)
    }
}

/// Assign trample excess damage when attacking a PW with trample-over-PW.
/// CR 702.19c: lethal to blocker(s) → loyalty-worth to PW → excess to PW controller.
fn assign_trample_over_pw_excess(
    state: &GameState,
    attacker_info: &crate::game::combat::AttackerInfo,
    excess: u32,
) -> Vec<DamageAssignment> {
    let mut result = Vec::new();
    if excess == 0 {
        return result;
    }
    let (pw_loyalty, _) = compute_pw_loyalty_threshold(state, &attacker_info.attack_target);
    let effective_loyalty = pw_loyalty.unwrap_or(0);
    let to_pw = excess.min(effective_loyalty);
    let to_controller = excess.saturating_sub(to_pw);

    if to_pw > 0 {
        // CR 702.19e: trample_over_pw=true so PW removal falls back to defending player.
        if let Some(target) = attacker_info.resolve_damage_target(state, true) {
            result.push(DamageAssignment {
                target,
                amount: to_pw,
            });
        }
    }
    if to_controller > 0 {
        result.push(DamageAssignment {
            target: DamageTarget::Player(attacker_info.defending_player),
            amount: to_controller,
        });
    }
    result
}

fn combat_damage_assignment_modes(
    obj: &GameObject,
    blocked: bool,
    has_blockers: bool,
    trample: Option<TrampleKind>,
) -> Vec<CombatDamageAssignmentMode> {
    if obj.assigns_damage_as_though_unblocked && blocked && (has_blockers || trample.is_none()) {
        vec![
            CombatDamageAssignmentMode::Normal,
            CombatDamageAssignmentMode::AsThoughUnblocked,
        ]
    } else {
        vec![CombatDamageAssignmentMode::Normal]
    }
}

pub(crate) fn assign_damage_as_though_unblocked(
    state: &GameState,
    attacker_info: &crate::game::combat::AttackerInfo,
    power: u32,
    trample: Option<TrampleKind>,
) -> Vec<DamageAssignment> {
    let is_over_pw = trample == Some(TrampleKind::OverPlaneswalkers);
    match attacker_info.resolve_damage_target(state, is_over_pw) {
        Some(target) => vec![DamageAssignment {
            target,
            amount: power,
        }],
        None => Vec::new(),
    }
}

/// Auto-assign damage for unblocked or single-blocker attackers.
/// Multi-blocker cases (2+) are handled interactively via WaitingFor::AssignCombatDamage.
fn assign_attacker_damage(
    state: &GameState,
    attacker_info: &crate::game::combat::AttackerInfo,
    combat: &CombatState,
    power: u32,
    has_deathtouch: bool,
    trample: Option<TrampleKind>,
) -> Vec<DamageAssignment> {
    let attacker_id = attacker_info.object_id;

    let blockers = combat
        .blocker_assignments
        .get(&attacker_id)
        .filter(|b| !b.is_empty());

    match blockers {
        None => {
            if attacker_info.blocked {
                // CR 702.19d: Trample (both variants) — blocked but no blockers remaining,
                // assign all damage to attack target as though lethal was assigned.
                if trample.is_some() {
                    let is_over_pw = trample == Some(TrampleKind::OverPlaneswalkers);
                    if is_over_pw
                        && matches!(attacker_info.attack_target, AttackTarget::Planeswalker(..))
                    {
                        // CR 702.19d + CR 702.19c: Trample-over-PW with no blockers attacking PW
                        return assign_trample_over_pw_excess(state, attacker_info, power);
                    }
                    // CR 702.19d: Standard trample with no blockers — all to attack target
                    match attacker_info.resolve_damage_target(state, false) {
                        Some(target) => {
                            return vec![DamageAssignment {
                                target,
                                amount: power,
                            }]
                        }
                        None => return Vec::new(),
                    }
                }
                // CR 509.1h + CR 510.1c: Non-trample blocked creature with all
                // blockers removed — still "blocked" and assigns no damage.
                return Vec::new();
            }
            // CR 510.1b: Unblocked creature assigns damage to the player/planeswalker/battle it's attacking.
            // CR 506.4c / CR 702.19e: If PW left, trample-over-PW falls back to defending player.
            let is_over_pw = trample == Some(TrampleKind::OverPlaneswalkers);
            match attacker_info.resolve_damage_target(state, is_over_pw) {
                Some(target) => vec![DamageAssignment {
                    target,
                    amount: power,
                }],
                None => Vec::new(),
            }
        }
        Some(blockers) => {
            if blockers.len() == 1 {
                if let Some(trample_kind) = trample {
                    // CR 702.19b: Single-blocker trample with excess is now routed to the
                    // interactive WaitingFor::AssignCombatDamage prompt by
                    // needs_interactive_assignment, so this auto-assign branch is reached only
                    // when power <= lethal (excess == 0). The excess sub-block below stays as
                    // the shared lethal-then-excess logic used by the no-blockers and PW paths.
                    // CR 702.19b: Trample — assign lethal to blocker, excess to attack target.
                    let lethal = lethal_damage_needed(state, blockers[0], has_deathtouch);
                    let to_blocker = power.min(lethal);
                    let excess = power.saturating_sub(to_blocker);
                    let mut result = vec![DamageAssignment {
                        target: DamageTarget::Object(blockers[0]),
                        amount: to_blocker,
                    }];
                    if excess > 0 {
                        if trample_kind == TrampleKind::OverPlaneswalkers
                            && matches!(attacker_info.attack_target, AttackTarget::Planeswalker(..))
                        {
                            // CR 702.19c: Trample-over-PW attacking PW — split excess
                            // between PW (up to loyalty) and PW controller.
                            result.extend(assign_trample_over_pw_excess(
                                state,
                                attacker_info,
                                excess,
                            ));
                        } else {
                            // CR 702.19f: Standard trample or trample-over-PW attacking
                            // non-PW — excess goes to the attack target directly.
                            if let Some(target) = attacker_info.resolve_damage_target(state, false)
                            {
                                result.push(DamageAssignment {
                                    target,
                                    amount: excess,
                                });
                            }
                        }
                    }
                    result
                } else {
                    // Single blocker without trample: all damage to blocker
                    vec![DamageAssignment {
                        target: DamageTarget::Object(blockers[0]),
                        amount: power,
                    }]
                }
            } else {
                // 2+ blockers: handled interactively via WaitingFor::AssignCombatDamage.
                // This branch should never be reached — needs_interactive_assignment
                // returns true for 2+ blockers and collect_damage_assignments pauses.
                debug_assert!(false, "multi-blocker auto-assignment should not be reached");
                Vec::new()
            }
        }
    }
}

/// How much combat damage `attacker_id` assigns to the defending player over the
/// whole combat damage phase if exactly `blockers` block it.
///
/// The single authority for "does this block save me". Consumers must not
/// re-derive it: every hand-rolled version of this calculation has gotten one of
/// the rules below wrong, because they interact.
///
/// - CR 510.1a: a creature assigns damage equal to its power; 0 or less assigns none.
/// - CR 510.1b: an empty `blockers` slice means unblocked — all power to the player.
/// - CR 510.1c: a blocked creature assigns to its blockers, and none to the player.
/// - CR 702.19b: trample assigns lethal to every blocker first, then the excess to
///   the player — and *no* damage to the player if lethal is not assigned to all of
///   them. "Lethal" counts damage already marked (via `lethal_damage_needed`).
/// - CR 702.19d: a blocked trampler with no blockers left when damage is assigned
///   assigns everything to the player, as though all blockers had been assigned
///   lethal damage. This is what makes the second strike of a double striker so
///   dangerous: the first strike step usually empties the block.
/// - CR 702.4b + CR 702.7b: first and double strike split combat into two damage
///   steps. Blockers killed in the first step are gone for the second, and a
///   first-striking blocker can kill the attacker before its regular-step damage.
/// - CR 702.2c: deathtouch makes 1 damage lethal, so a deathtouch trampler is
///   absorbed by almost nothing.
///
/// Assignment within a step is resolved as the worst case for the defending
/// player, since the attacking player chooses (CR 702.19b): lethal is assigned to
/// the cheapest blockers first, maximising how many die and therefore how little
/// absorption survives into the second step.
pub fn combat_damage_to_defender(
    state: &GameState,
    attacker_id: ObjectId,
    blockers: &[ObjectId],
) -> i32 {
    let Some(attacker) = state.objects.get(&attacker_id) else {
        return 0;
    };
    // CR 510.1a + CR 613.11: `combat_damage_amount` is the resolver's authority for
    // how much a creature assigns — power normally, toughness under a Doran-style
    // rule-modifying effect, and 0 under "assigns no combat damage". Reading raw
    // power here would make this helper contradict the combat it exists to predict.
    // CR 510.1a: a creature assigning 0 or less assigns no combat damage at all.
    let power = combat_damage_amount(attacker) as i32;
    if power == 0 {
        return 0;
    }
    let has_trample = attacker.has_keyword(&Keyword::Trample);
    let has_deathtouch = attacker.has_keyword(&Keyword::Deathtouch);
    let has_double_strike = attacker.has_keyword(&Keyword::DoubleStrike);
    let attacker_strikes_first = has_double_strike || attacker.has_keyword(&Keyword::FirstStrike);

    // CR 510.1b + CR 702.4b: unblocked — the whole power reaches the player, once
    // per damage step it assigns in. A double striker assigns in both, which is what
    // `resolve_combat_damage` does (`double_strike_deals_damage_twice`: an unblocked
    // 3/3 double striker deals 6). Plain first strike assigns in one step only.
    if blockers.is_empty() {
        return if has_double_strike { power * 2 } else { power };
    }

    // Remaining lethal per blocker, in the order the attacker would exhaust them.
    let mut remaining: Vec<i32> = blockers
        .iter()
        .map(|&bid| lethal_damage_needed(state, bid, has_deathtouch) as i32)
        .collect();
    remaining.sort_unstable();

    // CR 702.7b: which blockers assign their damage in the first-strike step.
    let first_strike_blockers: Vec<&GameObject> = blockers
        .iter()
        .filter_map(|bid| state.objects.get(bid))
        .filter(|b| b.has_keyword(&Keyword::FirstStrike) || b.has_keyword(&Keyword::DoubleStrike))
        .collect();

    let mut to_player = 0;
    let mut attacker_alive = true;

    // CR 702.4b: the first combat damage step exists at all only if some
    // participant has first or double strike.
    if attacker_strikes_first || !first_strike_blockers.is_empty() {
        if attacker_strikes_first {
            to_player += assign_step_to_defender(&mut remaining, power, has_trample);
        }
        // CR 702.7b: first-striking blockers assign now, and can kill the attacker
        // before it ever reaches the regular step.
        let attacker_lethal = lethal_damage_needed(
            state,
            attacker_id,
            first_strike_blockers
                .iter()
                .any(|b| b.has_keyword(&Keyword::Deathtouch)),
        ) as i32;
        // CR 510.1a + CR 613.11: same authority for the blockers — a first-striking
        // Doran-style blocker kills the attacker with its toughness, and one that
        // assigns no combat damage cannot kill it at all.
        let first_strike_damage: i32 = first_strike_blockers
            .iter()
            .map(|b| combat_damage_amount(b) as i32)
            .sum();
        if first_strike_damage >= attacker_lethal {
            attacker_alive = false;
        }
    }

    // CR 702.4b: the attacker assigns in the regular step if it has double strike,
    // or if it never had first strike and so has not assigned at all yet.
    if attacker_alive && (has_double_strike || !attacker_strikes_first) {
        to_player += assign_step_to_defender(&mut remaining, power, has_trample);
    }

    to_player
}

/// One combat damage step's worth of assignment from a single attacker, returning
/// the damage that reaches the defending player and consuming the blockers it
/// kills. See `combat_damage_to_defender` for the governing rules.
fn assign_step_to_defender(remaining: &mut Vec<i32>, power: i32, has_trample: bool) -> i32 {
    if remaining.is_empty() {
        // CR 702.19d: blocked, but nothing is blocking it now — a trampler assigns
        // everything to the player. CR 510.1c: without trample it assigns nothing.
        return if has_trample { power } else { 0 };
    }

    let total_lethal: i32 = remaining.iter().sum();

    // CR 702.19b: excess reaches the player only once EVERY blocker has been
    // assigned lethal damage.
    if has_trample && power > total_lethal {
        remaining.clear();
        return power - total_lethal;
    }

    // Otherwise every point is spent on blockers. Kill the cheapest first — the
    // attacking player chooses, and fewer survivors means less absorption in any
    // later step.
    //
    // CR 702.19b: damage assigned to a blocker that survives stays MARKED on it, so
    // its lethal minimum in a later damage step is reduced by that much. Dropping
    // the leftover budget instead understated the defender's exposure: a 5-power
    // double-strike trampler against one 0/6 was read as 0 through, when the first
    // step marks 5 and the regular step then needs only 1 more before trampling 4.
    // `remaining` is sorted ascending, so the front is always the cheapest kill.
    let mut budget = power;
    while let Some(&lethal) = remaining.first() {
        if budget >= lethal {
            budget -= lethal;
            remaining.remove(0);
        } else {
            remaining[0] -= budget;
            break;
        }
    }
    0
}

/// How much damage is needed to kill this creature.
/// CR 702.2c: Deathtouch — any amount of damage from a deathtouch source is lethal.
///
/// Public because this is the single authority for "how much does this blocker
/// absorb": CR 702.19b requires a trampling attacker to assign this much to each
/// blocker before any excess is assigned to the player, so a consumer estimating
/// trample absorption must read the same number this resolver assigns rather than
/// re-deriving it from raw toughness and silently ignoring damage already marked.
pub fn lethal_damage_needed(
    state: &GameState,
    object_id: ObjectId,
    source_has_deathtouch: bool,
) -> u32 {
    if source_has_deathtouch {
        // CR 702.2c + CR 702.19b: With deathtouch, 1 damage is lethal.
        return 1;
    }
    state
        .objects
        .get(&object_id)
        .map(|obj| {
            let toughness = obj.toughness.unwrap_or(0).max(0) as u32;
            toughness.saturating_sub(obj.damage_marked)
        })
        .unwrap_or(1)
}

/// CR 510.2: One source's place in a simultaneous combat-damage batch — the
/// damage context, commander flag, and original assignment, carried alongside
/// the source's `ProposedEvent` so Phase C can apply the post-replacement
/// survivor and run combat-only bookkeeping.
struct BatchEntry<'a> {
    ctx: DamageContext,
    source_is_commander: bool,
    assignment: &'a DamageAssignment,
    /// CR 119.3 + CR 702.15b: when the source has lifelink, the controller it
    /// gains life for. `ctx.has_lifelink` is forced off so the per-assignment
    /// apply path does NOT emit a life-gain event; instead the whole batch from
    /// one source is summed and gained once (see Phase C) so "whenever you gain
    /// life" triggers fire once per source, not once per damaged target.
    lifelink_controller: Option<PlayerId>,
}

/// CR 510.2: one source's place in a combat-damage batch, hydrated for the
/// shared completion tail — owned on both paths (cloned from the inline
/// borrowed `BatchEntry`, rebuilt from the parked snapshot on resume).
struct HydratedCombatEntry {
    ctx: DamageContext,
    source_is_commander: bool,
    assignment: DamageAssignment,
    lifelink_controller: Option<PlayerId>,
}

/// CR 510.2 + CR 615.13 + CR 120.3: the completion tail shared by the inline
/// path and the CR 616.1 resume so the two cannot drift — tally merge/close/
/// restore, Phase C survivor application + combat bookkeeping, the per-player
/// aggregate, and the `drain_combat_lifelink` call (gains, aggregate, Phase D
/// riders, ring guard).
///
/// `phase_a_tally` is the live Phase-A window's contents and `phase_b_tally`
/// the batch run's aggregate; the resume passes its combined tally as the
/// Phase-A leg with an empty Phase-B leg (merging is keyed sums, so one leg +
/// empty equals the inline two-leg merge). `events` is the batch's events so
/// far (inline rider/drain events, plus every folded answer span on resume).
#[allow(clippy::too_many_arguments)]
fn apply_combat_survivors(
    state: &mut GameState,
    entries: Vec<HydratedCombatEntry>,
    survivors: Vec<Option<ProposedEvent>>,
    phase_a_tally: CombatPreventionTally,
    phase_b_tally: CombatPreventionTally,
    outer_tally: Option<CombatPreventionTally>,
    lives_before: Vec<i32>,
    sub_step: CombatDamageSubStep,
    mut events: Vec<GameEvent>,
) -> CombatDamageBatch {
    // CR 510.2: accumulates per-player, per-source damage for this step only.
    // `(player, [(source_id, amount)], step_total)`.
    type PerPlayerCombatDamage = (crate::types::player::PlayerId, Vec<(ObjectId, u32)>, u32);
    let mut combat_damage_to_players: Vec<PerPlayerCombatDamage> = Vec::new();
    // CR 400.7: each source's incarnation as it dealt combat damage this step.
    let mut source_incarnations: Vec<crate::types::identifiers::ObjectIncarnationRef> = Vec::new();
    // CR 119.3 + CR 702.15b: per-source lifelink life gain summed across this
    // simultaneous batch — `(source_id, controller, total_dealt)`. Applied once
    // per source after the batch so "whenever you gain life" triggers once.
    let mut lifelink_by_source: Vec<(ObjectId, PlayerId, u32)> = Vec::new();

    // CR 510.2 + CR 615.13 (Matt R7-F1): Phase B ran under its own fresh tally
    // (restoring the Phase-A window on exit), so merge its aggregate into the
    // Phase-A protection tally — the boundary publishes ONE aggregate per
    // (shield, recipient) for the whole step. Then close this step's window
    // and restore any outer tally untouched.
    let mut prevention_tally = phase_a_tally;
    for (key, amount) in phase_b_tally {
        *prevention_tally.entry(key).or_insert(0) += amount;
    }
    state.combat_prevention_tally = outer_tally;

    // --- Phase C: Apply survivors + combat bookkeeping (CR 120.3 + CR 510.2) ---
    debug_assert_eq!(
        entries.len(),
        survivors.len(),
        "batch survivor count must align with collected entries"
    );
    for (entry, survivor) in entries.iter().zip(survivors) {
        let actual_amount = match survivor {
            // CR 120.3 + CR 120.4b: Apply the post-replacement event WITHOUT
            // re-running the replacement pipeline.
            Some(survivor_event) => match apply_damage_after_replacement(
                state,
                &entry.ctx,
                survivor_event,
                true,
                &mut events,
            ) {
                DamageResult::Applied(amt) => amt,
                // CR 510.2: Life-loss/lifelink replacement needs a choice
                // mid-application, but that sub-pause point is unmodeled — the
                // deferred portion is dropped (the CR 616.1 ordering pause parks
                // before Phase C; it cannot serve a choice raised here)
                // (mirrors the legacy `apply_damage_to_target` combat behavior).
                DamageResult::NeedsChoice => 0,
            },
            // Fully prevented or skipped — no damage applied.
            None => 0,
        };

        // CR 119.3 + CR 702.15b: accumulate this source's lifelink over the batch
        // (damage to any recipient — creature, planeswalker, or player — feeds the
        // same life gain). Keyed by source so two different lifelink sources stay
        // separate events while one source hitting many targets is summed.
        if let Some(controller) = entry.lifelink_controller {
            if actual_amount > 0 {
                let source_id = entry.ctx.source_id;
                if let Some((_, _, total)) = lifelink_by_source
                    .iter_mut()
                    .find(|(id, _, _)| *id == source_id)
                {
                    *total += actual_amount;
                } else {
                    lifelink_by_source.push((source_id, controller, actual_amount));
                }
            }
        }

        // Combat-only bookkeeping (not part of the shared damage pipeline):
        if let DamageTarget::Player(player_id) = &entry.assignment.target {
            let source_id = entry.ctx.source_id;
            // CR 510.2: Track per-source amounts for this step. Each source
            // appears at most once per player per step; dedup guards any edge
            // where the same source is re-applied (e.g. split-damage riders).
            if let Some(incarnation) = entry.ctx.source_incarnation {
                let source =
                    crate::types::identifiers::ObjectIncarnationRef::of(source_id, incarnation);
                if !source_incarnations.contains(&source) {
                    source_incarnations.push(source);
                }
            }
            if let Some((_, sources, total)) = combat_damage_to_players
                .iter_mut()
                .find(|(damaged_player, _, _)| *damaged_player == *player_id)
            {
                if let Some((_, amt)) = sources.iter_mut().find(|(id, _)| *id == source_id) {
                    *amt += actual_amount;
                } else {
                    sources.push((source_id, actual_amount));
                }
                *total += actual_amount;
            } else {
                combat_damage_to_players.push((
                    *player_id,
                    vec![(source_id, actual_amount)],
                    actual_amount,
                ));
            }

            // CR 704.6c: Track commander combat damage for the 21-damage loss condition.
            if entry.source_is_commander && actual_amount > 0 {
                if let Some(entry) = state
                    .commander_damage
                    .iter_mut()
                    .find(|e| e.player == *player_id && e.commander == source_id)
                {
                    entry.damage += actual_amount;
                } else {
                    state
                        .commander_damage
                        .push(crate::types::game_state::CommanderDamageEntry {
                            player: *player_id,
                            commander: source_id,
                            damage: actual_amount,
                        });
                }
            }
        }
    }

    // CR 510.2: the per-player aggregate, built here in exactly the order it has
    // always been emitted and carried to the drain so a pause cannot reorder it
    // ahead of a resumed gain.
    let damage_to_players: Vec<GameEvent> = combat_damage_to_players
        .into_iter()
        .map(|(player_id, source_amounts, total_damage)| {
            // CR 400.7: the incarnations of this player's sources.
            let source_incarnations = source_incarnations
                .iter()
                .filter(|source| source_amounts.iter().any(|(id, _)| *id == source.object_id))
                .copied()
                .collect();
            GameEvent::CombatDamageDealtToPlayer {
                player_id,
                source_amounts,
                total_damage,
                source_incarnations,
            }
        })
        .collect();

    // CR 616.1 + CR 702.15e: each surviving source's lifelink gain is its own
    // event. The whole tail — gains, the per-player aggregate, Phase D's
    // prevention riders, and the CR 732.2a ring guard — lives in the drain, in
    // the order it has always been emitted, so a CR 616.1 pause can suspend it
    // without reordering it.
    // Deterministic order: the tally arrives as a `HashMap`, but it is stored in
    // `PendingCombatLifelink` (which is `Serialize`/`Deserialize`, so a parked
    // batch must round-trip byte-identically) and Phase D fires one CR 615.5
    // rider per entry IN THIS ORDER. A `HashMap`'s arbitrary iteration order
    // would let the same batch serialize — and emit its rider events —
    // differently across saves and AI clones. The ids are not `Ord`, so sort on
    // their inner integers, exactly as `blocker_ids` is sorted above.
    let prevention_tally = sorted_prevention_entries(prevention_tally);

    drain_combat_lifelink(
        state,
        PendingCombatLifelink {
            remaining: lifelink_by_source
                .into_iter()
                .map(|(_source_id, controller, amount)| PendingLifelinkGain { controller, amount })
                .collect(),
            batch_events: events,
            damage_to_players,
            prevention_tally,
            lives_before,
            sub_step,
        },
    )
}

/// Apply all combat damage assignments simultaneously (CR 510.2).
///
/// Combat damage is one simultaneous event batch (CR 510.2). To keep prevention
/// shields (e.g. Inkshield's "prevent all combat damage ... for each 1 damage
/// prevented, create a token") rules-correct (CR 615.7 — count the amount, not
/// the sources; CR 615.13 — the rider fires once per batch), this runs in
/// three phases:
///
/// - **Phase A (Collect):** build a `ProposedEvent::Damage` per assignment
///   through `pre_replacement_damage_gate` (0-damage / protection / prohibition
///   gates) WITHOUT applying any damage yet.
/// - **Phase B (Batch replace):** pass the whole proposed-event slice through
///   `replace_combat_damage_batch`, which runs the replacement pipeline per
///   event but aggregates each `Prevention::All` shield's prevented amount.
/// - **Phase C (Apply + bookkeeping):** apply each surviving post-replacement
///   event via `apply_damage_after_replacement`, then run the player-batching
///   and commander-damage bookkeeping. Afterward, fire each prevention shield's
///   `runtime_execute` rider exactly once against the aggregate prevented
///   amount (CR 615.5 + CR 615.13).
///
/// Used by both the automatic assignment path and the interactive
/// `AssignCombatDamage` handler.
/// CR 510.2: the outcome of applying one simultaneous combat-damage batch.
///
/// An enum rather than a `Vec<GameEvent>` plus an out-param: the previous return
/// type had no channel for a pause, which is exactly why the pause was silently
/// discarded. A caller now cannot ignore it without a compile error.
pub(crate) enum CombatDamageBatch {
    /// Every assignment applied, every lifelink gain completed, and Phase D fired.
    Complete(Vec<GameEvent>),
    /// CR 616.1: the batch met two or more co-applicable replacement effects
    /// and the affected player must choose which applies first. Either a
    /// lifelink life-gain paused (all damage already dealt; the unfinished
    /// tail is parked in `state.pending_combat_lifelink` and
    /// `state.pending_replacement` holds the paused source's own `LifeGain`
    /// as the resume's authority) or a combat-damage event paused before
    /// application (nothing dealt; the whole pre-application batch is parked
    /// in `state.pending_combat_damage` and `state.pending_replacement` holds
    /// the paused `Damage` event as the resume's authority).
    Paused {
        events: Vec<GameEvent>,
        /// Boxed: `WaitingFor` is ~1720 bytes against `Complete`'s 24, so an
        /// inline field would make every `Complete` return pay the pause's size
        /// (`clippy::large_enum_variant`).
        waiting_for: Box<WaitingFor>,
    },
}

impl CombatDamageBatch {
    /// The batch's events so far, for the caller's running event log.
    fn events(&self) -> &[GameEvent] {
        match self {
            Self::Complete(events) | Self::Paused { events, .. } => events,
        }
    }
}

pub(crate) fn apply_combat_damage(
    state: &mut GameState,
    assignments: &[(ObjectId, DamageAssignment)],
    sub_step: CombatDamageSubStep,
) -> CombatDamageBatch {
    let mut events = Vec::new();
    // CR 510.2 + CR 732.2a: the pre-batch life totals, so the loop-detection ring can be
    // invalidated on the DAMAGE EVENT rather than on a `WaitingFor` window that an
    // unblocked attacker never opens. Snapshotted per batch, not hoisted: first-strike
    // and regular damage are two separate CR 510.2 events (`:131` / `:188`) and a
    // double-strike attacker must be caught at the first, not only the second. See
    // `GameState::invalidate_loop_ring_on_unobserved_life_move`.
    let lives_before: Vec<i32> = state.players.iter().map(|p| p.life).collect();

    // --- Phase A: Collect proposed damage events (CR 510.2) ---
    // CR 702.16e + CR 510.2 (Matt R7-F1): the batch tally window opens BEFORE
    // Phase A so protection prevention accumulates into this step's aggregate
    // instead of emitting per-source. Saved/restored, never merged outward:
    // combat is its own simultaneity even if an outer batch is live.
    let outer_tally = state.combat_prevention_tally.take();
    state.combat_prevention_tally = Some(HashMap::new());
    // Gated assignments (0-damage, protection, prohibition) contribute nothing
    // and are dropped here; the gate accumulates any required DamagePrevented
    // into the tally above for the boundary publisher.
    let mut entries: Vec<BatchEntry> = Vec::with_capacity(assignments.len());
    let mut proposed_events: Vec<ProposedEvent> = Vec::with_capacity(assignments.len());
    for (source_id, assignment) in assignments {
        // Read commander flag before DamageContext borrows — both are immutable reads.
        let source_is_commander = state
            .objects
            .get(source_id)
            .map(|o| o.is_commander)
            .unwrap_or(false);
        // In practice, from_source always succeeds during combat (source is on battlefield).
        // CR 702.15c: Fallback uses LKI controller when the source is gone.
        let mut ctx = DamageContext::from_source(state, *source_id).unwrap_or_else(|| {
            let controller = state
                .lki_cache
                .get(source_id)
                .map(|lki| lki.controller)
                .unwrap_or(state.active_player);
            DamageContext::fallback(*source_id, controller)
        });

        // CR 119.3 + CR 702.15b: defer lifelink to a single per-source life-gain
        // after the whole simultaneous batch is applied. A source dealing combat
        // damage to several recipients at once (multiple blockers, trample to a
        // blocker + the player, etc.) gains life ONCE for the total — so a
        // "whenever you gain life" trigger fires once, not once per target.
        let lifelink_controller = if ctx.has_lifelink {
            Some(ctx.controller)
        } else {
            None
        };
        ctx.has_lifelink = false;

        let target_ref = match &assignment.target {
            DamageTarget::Object(id) => TargetRef::Object(*id),
            DamageTarget::Player(id) => TargetRef::Player(*id),
        };

        // CR 702.16e + CR 615.13 (Matt F4, aggregated by R7-F1): protection
        // prevention aggregates through the shared helper into the tally
        // window opened above, publishing once per recipient at the boundary.
        match pre_replacement_damage_gate(state, &ctx, &target_ref, assignment.amount, true) {
            DamageGateOutcome::Proposed(proposed) => {
                entries.push(BatchEntry {
                    ctx,
                    source_is_commander,
                    assignment,
                    lifelink_controller,
                });
                proposed_events.push(proposed);
            }
            DamageGateOutcome::Protected(prevented, quality) => {
                emit_or_accumulate_protection_prevention(
                    state,
                    ctx.source_id,
                    &target_ref,
                    prevented,
                    quality,
                    &mut events,
                );
            }
            DamageGateOutcome::Gated => {}
        }
    }

    // --- Phase B: Batch replacement (CR 510.2 + CR 615.7) ---
    match replacement::replace_combat_damage_batch(state, &mut events, proposed_events) {
        replacement::CombatDamageReplacementOutcome::Complete {
            survivors,
            tally: phase_b_tally,
        } => {
            let phase_a_tally = state.combat_prevention_tally.take().unwrap_or_default();
            let hydrated: Vec<HydratedCombatEntry> = entries
                .into_iter()
                .map(|entry| HydratedCombatEntry {
                    ctx: entry.ctx,
                    source_is_commander: entry.source_is_commander,
                    assignment: entry.assignment.clone(),
                    lifelink_controller: entry.lifelink_controller,
                })
                .collect();
            apply_combat_survivors(
                state,
                hydrated,
                survivors,
                phase_a_tally,
                phase_b_tally,
                outer_tally,
                lives_before,
                sub_step,
                events,
            )
        }
        replacement::CombatDamageReplacementOutcome::Paused {
            decided,
            remaining,
            tally: phase_b_tally,
            player,
        } => {
            // CR 616.1: one event's pipeline needs an ordering choice. NOTHING
            // has been applied yet (Phase C never ran) — park the whole
            // pre-application batch and prompt the affected player. The paused
            // event is `entries[decided.len()]`: consumed from the batch
            // iterator, so it is in neither `decided` nor `remaining`, and
            // `state.pending_replacement` (left populated by the batch fn) is
            // the resume's authority for it.
            debug_assert!(
                state.pending_combat_lifelink.is_none(),
                "single-park invariant: the damage record parks before the lifelink drain runs"
            );
            let paused_entry = decided.len();
            debug_assert_eq!(
                entries.len(),
                paused_entry + 1 + remaining.len(),
                "paused slot must sit exactly between decided and remaining"
            );
            let phase_a_tally =
                tally_entries_for_record(&state.combat_prevention_tally.take().unwrap_or_default());
            let phase_b_tally = tally_entries_for_record(&phase_b_tally);
            let outer_entries = outer_tally.as_ref().map(tally_entries_for_record);
            state.combat_prevention_tally = outer_tally;
            let parked_entries: Vec<ParkedCombatDamageEntry> = entries
                .into_iter()
                .map(|entry| ParkedCombatDamageEntry {
                    ctx: DamageContextSnapshot::from(&entry.ctx),
                    source_is_commander: entry.source_is_commander,
                    assignment: entry.assignment.clone(),
                    lifelink_controller: entry.lifelink_controller,
                })
                .collect();
            // CR 510.2 + CR 616.1 (Matt R12): bind the paused member to this
            // prompt BEFORE the record parks, so the answer-stash gate keeps
            // working after an answered choice redirects the event.
            if let Some(entry) = parked_entries.get(paused_entry) {
                stamp_combat_pause_slot(state, paused_entry, entry.ctx.source_id);
            }
            // CR 732.2a: the batch may already have moved life (an inline
            // redirect or rider leg inside Phase B). Invalidate now as well as
            // at completion, against the SAME pre-batch snapshot. The guard
            // only ever CLEARS, so the paired calls are idempotent.
            state.invalidate_loop_ring_on_unobserved_life_move(&lives_before);
            let waiting_for = replacement::replacement_choice_waiting_for(player, state);
            state.waiting_for = waiting_for.clone();
            let returned = events.clone();
            state.pending_combat_damage = Some(Box::new(PendingCombatDamageBatch {
                entries: parked_entries,
                decided,
                paused_entry,
                paused_entry_pruned: false,
                remaining,
                phase_a_tally,
                phase_b_tally,
                outer_tally: outer_entries,
                batch_events: events,
                lives_before,
                sub_step,
            }));
            CombatDamageBatch::Paused {
                events: returned,
                waiting_for: Box::new(waiting_for),
            }
        }
    }
}

/// CR 616.1 + CR 702.15e + CR 615.5 + CR 732.2a: finish one simultaneous
/// combat-damage batch — apply each remaining lifelink source's life gain as its
/// own event, emit the per-player damage aggregate, fire Phase D's prevention
/// riders, and close the CR 732.2a window.
///
/// The gain goes through `effects::life::apply_life_gain`, the single authority
/// for a replacement-routed life change. When two or more co-applicable
/// replacements make the ordering material (CR 616.1), that authority parks
/// `state.pending_replacement`, sets `state.waiting_for` to the ordering prompt,
/// and returns `Err(ReplacementDeferred::ReplacementChoice)` having applied
/// NOTHING. The rest of the batch is parked here so the resume can finish it;
/// dropping it loses 100% of that source's gain, not a portion, plus every
/// later source in the batch.
fn drain_combat_lifelink(
    state: &mut GameState,
    mut record: PendingCombatLifelink,
) -> CombatDamageBatch {
    let waiting_before = state.waiting_for.clone();
    while let Some(gain) = record.remaining.pop_front() {
        match crate::game::effects::life::apply_life_gain(
            state,
            gain.controller,
            gain.amount,
            &mut record.batch_events,
        ) {
            Ok(_) => {}
            // CR 616.1: the gaining player must choose which replacement applies
            // first. Nothing was applied for this source. `state.pending_replacement`
            // is left populated ON PURPOSE — it is the resume's authority, and this
            // source is deliberately NOT re-queued into `remaining` (that would gain
            // the life twice).
            Err(ReplacementDeferred::ReplacementChoice) => {
                let waiting_for = state.waiting_for.clone();
                let events = record.batch_events.clone();
                // CR 732.2a: the batch has already moved life (CR 120.3a damage,
                // and any earlier source's gain). Invalidate now as well as at
                // completion, against the SAME pre-batch snapshot, so the ring is
                // not stale for the duration of an arbitrarily long player pause.
                // The guard only ever CLEARS, so the paired calls are idempotent.
                state.invalidate_loop_ring_on_unobserved_life_move(&record.lives_before);
                state.pending_combat_lifelink = Some(Box::new(record));
                return CombatDamageBatch::Paused {
                    events,
                    waiting_for: Box::new(waiting_for),
                };
            }
            // CR 614.6 + CR 614.1: a cross-event substitute (Lich class) already
            // ran its replacing action. On Execute the gain was APPLIED before
            // `drain_substitution_continuation` ran, and on Prevented nothing was
            // owed — so NO lifelink gain is outstanding on this arm and nothing
            // needs parking.
            //
            // strict-failure: restoring `waiting_for` here preserves the existing
            // behavior byte-for-byte, and that behavior orphans the substitute's
            // own interactive prompt. That is a PRE-EXISTING defect on the
            // substitution-continuation seam, which this change neither owns nor
            // worsens. The codebase distinguishes the two deferral kinds elsewhere
            // for exactly this reason (`turns.rs` calls
            // `mark_phase_transition_awaiting_post_replacement` only for the
            // substitution variant), so the divergence here is deliberate, not an
            // oversight.
            Err(ReplacementDeferred::SubstitutionContinuation { .. }) => {
                state.waiting_for = waiting_before.clone();
            }
        }
    }
    // CR 510.2: the per-player aggregate, emitted after the gains exactly as it
    // is on a batch that never paused.
    record.batch_events.append(&mut record.damage_to_players);
    // CR 615.5 + CR 615.13: Phase D fires once per shield, and only here — after
    // the last gain, so no rider continuation can ever run while a CR 616.1
    // prompt is open, and after nothing else can stamp `last_effect_count`.
    // Fire-all: combat keeps its standing shape (see the `stop_on_fresh_choice`
    // contract on `fire_batch_prevention_riders`).
    let _ = fire_batch_prevention_riders(
        state,
        &record.prevention_tally,
        &mut record.batch_events,
        false,
    );
    // CR 510.2 + CR 732.2a: the simultaneous batch is the EVENT the loop-ring life
    // prohibition keys on, and this is the last statement that can move life in it.
    // The batch moves life in three places and only a call here sees all three:
    // Phase C's `apply_damage_after_replacement` (CR 120.3a), the per-source
    // lifelink gains above, and a prevention rider's `runtime_execute` (CR 615.5).
    // `lives_before` is the PRE-BATCH snapshot carried in the record. On a resume
    // this window arithmetically spans the paused source's own gain, which
    // `engine_replacement.rs` applied before the drain was re-entered — but that
    // span has no observable consequence: `apply()` has already cleared
    // `loop_detect_ring` on the answering `ChooseReplacement`, which sits in
    // neither its action exemption list nor `WaitingFor::is_forced_cascade_window`.
    // MEASURED, not inferred. The load-bearing call is the PAUSE-path one above,
    // reached under the exempt `PassPriority`.
    state.invalidate_loop_ring_on_unobserved_life_move(&record.lives_before);
    CombatDamageBatch::Complete(record.batch_events)
}

/// CR 615.13: convert a `HashMap`-backed prevention tally into the deterministically
/// ordered entry list the batch drain fires. Shared by the combat batch (which
/// parks it in `PendingCombatLifelink`) and simultaneous noncombat batches
/// (`EachSourceDealsDamage`), so both aggregate identically.
///
/// `AppliedReplacementKey` is the PRIMARY sort rank, so each shield's entries
/// are contiguous and `chunk_by` in `fire_batch_prevention_riders` recovers
/// the whole recipient group as one CR 615.13 prevention application. The
/// CR 120.3 recipient (derived `Ord`) breaks ties inside a group so a parked
/// batch still serializes byte-identically across saves and AI clones.
pub(crate) fn sorted_prevention_entries(
    prevention_tally: CombatPreventionTally,
) -> Vec<PreventedDamageEntry> {
    let mut entries: Vec<PreventedDamageEntry> = prevention_tally
        .into_iter()
        .map(|((key, recipient), amount)| PreventedDamageEntry {
            key,
            recipient,
            amount,
        })
        .collect();
    entries.sort_by_key(|entry| {
        let rank = match entry.key {
            AppliedReplacementKey::Object { source, index } => (0u8, source.0, index, 0u8),
            AppliedReplacementKey::Floating { index } => (1, 0, index, 0),
            AppliedReplacementKey::StepEndMana { index } => (2, 0, index, 0),
            AppliedReplacementKey::EntryControllerChoice {
                source,
                index,
                controller,
            } => (3, source.0, index, controller.0),
            // Protection aggregates last; it has no rider and no registry
            // address, so its rank only needs determinism, not order meaning.
            // The matched quality rides whole (R8-F1): same quality stays
            // contiguous, disjoint qualities order deterministically — the
            // tally is a HashMap, so any rank tie would leak its iteration
            // order into the published application sequence.
            AppliedReplacementKey::Protection { quality } => {
                let (minor, discriminant) = quality.sort_words();
                (4, minor, 0, discriminant)
            }
        };
        (rank, entry.recipient.clone())
    });
    entries
}

/// CR 615.13 + CR 616.1e (Matt C2): what one aggregate drainage produced.
/// Either every group dispatched, or a rider parked a FRESH choice mid-frame:
/// `published_entries` is the leading entry count fully resolved (events
/// pushed, riders dispatched-or-skipped), so the caller can re-park the
/// unpublished remainder and stop unwinding instead of firing later groups
/// ahead of the pending choice.
pub(crate) enum BatchRiderDrainOutcome {
    Completed,
    Paused {
        waiting_for: Box<WaitingFor>,
        published_entries: usize,
    },
}

/// CR 615.5 + CR 615.13: After a simultaneous damage batch (combat or
/// noncombat), fire each `Prevention::All` shield's `runtime_execute` rider
/// exactly once against the aggregate prevented amount.
///
/// Each `DamagePrevented` event for the batch is emitted here (the per-source
/// applier suppressed them so the rider sees one un-fragmented amount), ONE per
/// (shield, recipient). CR 615.13 and CR 120.3 pull in different directions and
/// both are honored: the prevention APPLICATION is the trigger unit and the
/// CR 615.5 rider unit, so the rider fires once per shield against the batch
/// total, while the RECIPIENT is a per-event property, so a shield that
/// prevented damage for a player and a creature in the same batch emits one
/// event for each carrying that recipient's own amount. The aggregate prevented
/// amount is stamped into `last_effect_count` immediately before the single
/// rider call so the rider's `QuantityRef::EventContextAmount` (e.g. Inkshield's
/// "for each 1 damage prevented this way") resolves against the whole batch
/// total.
///
/// `stop_on_fresh_choice` (Matt C2, parked-frame drainage only): stop after
/// the first rider that parks a fresh choice and report it, so later groups
/// do not execute ahead of the pending prompt. The combat and unpaused-inline
/// callers pass `false` and keep the standing fire-all shape.
pub(crate) fn fire_batch_prevention_riders(
    state: &mut GameState,
    prevention_tally: &[PreventedDamageEntry],
    events: &mut Vec<GameEvent>,
    stop_on_fresh_choice: bool,
) -> BatchRiderDrainOutcome {
    // CR 615.13 + CR 120.3: the tally arrives sorted with each shield's entries
    // contiguous, so one chunk IS one prevention application over the CR 510.2
    // simultaneous batch. The application is the trigger unit and the CR 615.5
    // rider unit — fired ONCE below, against the group total. The RECIPIENT is
    // not: CR 120.3 makes "player or permanent" a property of each damage event,
    // and a shield's `damage_target_filter` is a predicate over recipients, not
    // the recipient of any one event. So one `DamagePrevented` is emitted per
    // recipient, carrying the amount prevented FOR THAT RECIPIENT — which is what
    // a trigger scoped to "damage that would be dealt to you" (Selfless Squire)
    // must read for its "that many" to be right.
    // Matt C2: the caller's prompt restored on completion, so any
    // non-Priority deviation after a dispatch is a fresh rider-created choice
    // (covers dispatches whose return is dropped on a nested path too).
    // Snapshotted only in stop mode; the fire-all callers never read it.
    let pre_drain_waiting = stop_on_fresh_choice.then(|| state.waiting_for.clone());
    let mut published_entries = 0usize;
    for group in prevention_tally.chunk_by(|a, b| a.key == b.key) {
        // Every group counts exactly once — resolved means events pushed (or
        // standing-behavior skipped), whether or not a rider dispatched.
        published_entries += group.len();
        let total_prevented: i32 = group.iter().map(|entry| entry.amount).sum();
        if total_prevented <= 0 {
            continue;
        }

        // CR 702.16e + CR 615.13 (Matt F4): protection prevention aggregates
        // under its own key — no registry shield, no rider, never a
        // `ReplacementId`. Publish the aggregate application and resolve the
        // group without dispatch. Sourceless like any floating prevention.
        if matches!(group[0].key, AppliedReplacementKey::Protection { .. }) {
            push_prevented_events(group, ObjectId(0), events);
            continue;
        }
        let rid = group[0].key.as_replacement_id();

        if replacement::is_shield_counter_damage_replacement(rid) {
            replacement::consume_shield_counter(state, rid.source, events);
            push_prevented_events(group, rid.source, events);
            continue;
        }

        // Pending (resolution-created) shields are stored in
        // `pending_damage_replacements` under the sentinel ObjectId(0) because
        // they have no host permanent; object-hosted shields are found in the
        // host's `replacement_definitions`.
        let repl_def = if rid.source == ObjectId(0) {
            rid.index_as_usize()
                .and_then(|i| state.pending_damage_replacements.get(i))
        } else {
            state
                .objects
                .get(&rid.source)
                .and_then(|obj| obj.replacement_definitions.get_by_rid(rid))
        };
        let Some(repl_def) = repl_def else {
            continue;
        };

        // CR 615.13: shields whose riders already drained inline per event
        // during the batch feed the PUBLIC aggregate event only — re-firing
        // here would run the rider a second time with the batch total. That
        // is the per-source reflectors (CR 615.5 binds each reflection to its
        // own damage source, which the aggregate cannot carry) and the
        // depletion/`AllBut` shields (whose applier arms keep the per-event
        // stash + inline drain in `replace_combat_damage_batch`).
        let rider_drained_inline = replacement::shield_rider_reflects_per_event(state, rid)
            || matches!(
                repl_def.shield_kind,
                crate::types::ability::ShieldKind::Prevention {
                    amount: crate::types::ability::PreventionAmount::Next(_)
                        | crate::types::ability::PreventionAmount::AllBut(_)
                }
            );

        // CR 615.5 + CR 609.7: A rider referencing the prevented damage's source
        // ("that source's controller" — New Way Forward) resolves
        // `PostReplacementSourceController` against the drain's event source. The
        // per-event `execute` path receives it from the Prevented-arm stash; the
        // aggregate path derives it here from the shield's resolved single-object
        // source filter (the chosen damage source).
        let event_source = repl_def
            .damage_source_filter
            .as_ref()
            .and_then(shield_specific_source);
        let runtime = repl_def.runtime_execute.clone();

        push_prevented_events(group, rid.source, events);

        // CR 615.5: Resolve the prevention's additional effect ("for each 1
        // damage prevented this way, create a token"). Stamp the aggregate
        // prevented amount so `EventContextAmount` resolves against the batch
        // total, then run the rider continuation exactly once — ONCE PER SHIELD,
        // not once per recipient, because CR 615.13 makes the application the
        // unit and CR 615.5's "that much damage was prevented" refers to the
        // whole application. Skipped for inline-drained riders (above): theirs
        // already fired per event during the batch.
        if rider_drained_inline {
            continue;
        }
        let Some(runtime) = runtime else {
            continue;
        };
        state.last_effect_count = Some(total_prevented);
        // Policy is `Replace`: this path has always overwritten a resident
        // continuation rather than deferring to it. The rider inherits no applied
        // set (it is the batch's own aggregate follow-up) — the fresh drain says
        // that by construction, where the old code had to remember to clear a
        // parallel field.
        state.install_ready_continuation(
            crate::types::ability::PostReplacementContinuation::Resolved(runtime),
        );
        if let Some(source) = event_source {
            if let Some(drain) = state
                .active_post_replacement_drains_mut()
                .and_then(crate::types::game_state::PostReplacementDrainStack::resident_mut)
            {
                drain.event_source = Some(source);
            }
        }
        let dispatch_wait = crate::game::engine_replacement::apply_pending_post_replacement_effect(
            state, None, None, None, events,
        );
        // Matt C2: in stop mode a fresh rider-created choice ends the frame
        // here — later groups must not execute ahead of the pending prompt.
        if stop_on_fresh_choice {
            let fresh_choice = dispatch_wait.or_else(|| {
                let pre = pre_drain_waiting.as_ref()?;
                (!matches!(state.waiting_for, WaitingFor::Priority { .. })
                    && state.waiting_for != *pre)
                    .then(|| state.waiting_for.clone())
            });
            if let Some(waiting_for) = fresh_choice {
                return BatchRiderDrainOutcome::Paused {
                    waiting_for: Box::new(waiting_for),
                    published_entries,
                };
            }
        }
    }
    BatchRiderDrainOutcome::Completed
}

/// CR 120.3 + CR 615.13: one `DamagePrevented` per recipient this shield
/// prevented damage for in the batch. Shared by the shield-counter branch and the
/// ordinary prevention branch so the two cannot drift.
///
/// `source_id` is the SHIELD's host object (the sentinel `ObjectId(0)` for a
/// floating shield), a pinned convention rather than a threaded value: CR 615.13
/// makes one prevention application span "one or more simultaneous damage
/// events", and under CR 510.2 those events routinely have DIFFERENT sources, so
/// no single damage source exists for the aggregate to name. Every per-event
/// emitter puts the damage source there instead; that divergence is deliberate
/// and unresolved, not an endorsement.
fn push_prevented_events(
    group: &[PreventedDamageEntry],
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    for entry in group {
        // CR 120.8 + CR 615.13: a prevention that prevented nothing for this
        // recipient is not a prevention event. The group total is already known
        // positive, so this guard only ever suppresses a zero recipient sharing a
        // batch with a nonzero one.
        if entry.amount <= 0 {
            continue;
        }
        events.push(GameEvent::DamagePrevented {
            source_id,
            target: entry.recipient.clone(),
            amount: entry.amount as u32,
        });
    }
}

/// CR 615.5 + CR 609.7: Extract the single damaging object a prevention shield is
/// scoped to, when its resolved `damage_source_filter` pins one specific object.
/// New Way Forward's chosen source resolves to `SpecificObject` (possibly ANDed
/// with a typed recheck), so the aggregate rider can populate the drain's event
/// source and resolve `PostReplacementSourceController` ("that source's
/// controller") against the prevented damage's dealer.
fn shield_specific_source(filter: &crate::types::ability::TargetFilter) -> Option<ObjectId> {
    use crate::types::ability::TargetFilter;
    match filter {
        TargetFilter::SpecificObject { id } => Some(*id),
        TargetFilter::And { filters } => filters.iter().find_map(shield_specific_source),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackerInfo, CombatState};
    use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, Comparator, ContinuousModification, ControllerRef, DamageModification,
        Effect, QuantityExpr, QuantityRef, ReplacementDefinition, StaticCondition,
        StaticDefinition, TargetFilter, TriggerDefinition, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::identifiers::{CardId, ObjectIncarnationRef, TrackedSetId};
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;
    use std::sync::Arc;

    fn setup() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state
    }

    fn create_creature(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.entered_battlefield_turn = Some(1);
        id
    }

    fn setup_combat(
        state: &mut GameState,
        attackers: Vec<ObjectId>,
        blocker_assignments: Vec<(ObjectId, Vec<ObjectId>)>,
    ) {
        let mut combat = CombatState {
            attackers: attackers
                .iter()
                .map(|&id| AttackerInfo::attacking_player(id, PlayerId(1)))
                .collect(),
            ..Default::default()
        };
        for (attacker_id, blockers) in blocker_assignments {
            // CR 509.1h: Mark the attacker as blocked.
            if let Some(info) = combat
                .attackers
                .iter_mut()
                .find(|a| a.object_id == attacker_id)
            {
                if !blockers.is_empty() {
                    info.blocked = true;
                }
            }
            for &blocker_id in &blockers {
                combat
                    .blocker_to_attacker
                    .entry(blocker_id)
                    .or_default()
                    .push(attacker_id);
            }
            combat.blocker_assignments.insert(attacker_id, blockers);
        }
        state.combat = Some(combat);
    }

    fn current_incarnation(state: &GameState, object_id: ObjectId) -> ObjectIncarnationRef {
        ObjectIncarnationRef::from_object(&state.objects[&object_id])
    }

    #[test]
    fn assess_combat_impact_reports_fixed_damage_and_survival_without_mutation() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Attacker", 3, 3);
        let blocker = create_creature(&mut state, PlayerId(1), "Blocker", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);
        let attacker_ref = current_incarnation(&state, attacker);
        let blocker_ref = current_incarnation(&state, blocker);
        let state_before = state.clone();
        let index_before = (
            state.replacement_index.initialized,
            state.replacement_index.dirty,
            state.replacement_index.pipeline_active,
            state.replacement_index.by_event.clone(),
        );

        assert_eq!(
            assess_combat_impact(&state, attacker_ref, blocker_ref),
            CombatImpact::Fixed {
                damage: FixedCombatDamage {
                    attacker_to_blocker: 3,
                    blocker_to_attacker: 2,
                },
                survival: FixedCombatSurvival {
                    attacker_survives: true,
                    blocker_survives: false,
                },
            }
        );
        assert_eq!(
            state, state_before,
            "assessment must not apply damage or mutate state"
        );
        assert_eq!(
            (
                state.replacement_index.initialized,
                state.replacement_index.dirty,
                state.replacement_index.pipeline_active,
                state.replacement_index.by_event.clone(),
            ),
            index_before,
            "assessment must not prepare or rebuild the replacement index"
        );
    }

    #[test]
    fn assess_combat_impact_is_indeterminate_when_damage_has_a_replacement() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Attacker", 3, 3);
        let blocker = create_creature(&mut state, PlayerId(1), "Blocker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .damage_modification(DamageModification::Double),
            );
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);
        let attacker_ref = current_incarnation(&state, attacker);
        let blocker_ref = current_incarnation(&state, blocker);
        let state_before = state.clone();

        assert_eq!(
            assess_combat_impact(&state, attacker_ref, blocker_ref),
            CombatImpact::Indeterminate
        );
        assert_eq!(
            state, state_before,
            "candidate scanning must not consume or apply the replacement"
        );
    }

    #[test]
    fn assess_combat_impact_rejects_stale_bound_combatant() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Attacker", 3, 3);
        let blocker = create_creature(&mut state, PlayerId(1), "Blocker", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);
        let stale_attacker = current_incarnation(&state, attacker);
        state.objects.get_mut(&attacker).unwrap().bump_incarnation();

        assert_eq!(
            assess_combat_impact(&state, stale_attacker, current_incarnation(&state, blocker)),
            CombatImpact::Indeterminate
        );
    }

    #[test]
    fn assess_combat_impact_fails_closed_for_complex_combat() {
        let mut multi_block_state = setup();
        let attacker = create_creature(&mut multi_block_state, PlayerId(0), "Attacker", 3, 3);
        let blocker_a = create_creature(&mut multi_block_state, PlayerId(1), "Blocker A", 2, 2);
        let blocker_b = create_creature(&mut multi_block_state, PlayerId(1), "Blocker B", 2, 2);
        setup_combat(
            &mut multi_block_state,
            vec![attacker],
            vec![(attacker, vec![blocker_a, blocker_b])],
        );
        assert_eq!(
            assess_combat_impact(
                &multi_block_state,
                current_incarnation(&multi_block_state, attacker),
                current_incarnation(&multi_block_state, blocker_a),
            ),
            CombatImpact::Indeterminate,
            "multiple blockers require a damage-assignment decision"
        );

        let mut keyword_state = setup();
        let keyword_attacker = create_creature(&mut keyword_state, PlayerId(0), "Attacker", 3, 3);
        let keyword_blocker = create_creature(&mut keyword_state, PlayerId(1), "Blocker", 2, 2);
        keyword_state
            .objects
            .get_mut(&keyword_attacker)
            .unwrap()
            .keywords
            .push(Keyword::FirstStrike);
        setup_combat(
            &mut keyword_state,
            vec![keyword_attacker],
            vec![(keyword_attacker, vec![keyword_blocker])],
        );
        assert_eq!(
            assess_combat_impact(
                &keyword_state,
                current_incarnation(&keyword_state, keyword_attacker),
                current_incarnation(&keyword_state, keyword_blocker),
            ),
            CombatImpact::Indeterminate,
            "first strike creates a separate combat-damage substep"
        );

        for keyword in [
            Keyword::DoubleStrike,
            Keyword::Deathtouch,
            Keyword::Wither,
            Keyword::Infect,
            Keyword::Trample,
        ] {
            let mut state = setup();
            let attacker = create_creature(&mut state, PlayerId(0), "Attacker", 3, 3);
            let blocker = create_creature(&mut state, PlayerId(1), "Blocker", 2, 2);
            state
                .objects
                .get_mut(&attacker)
                .unwrap()
                .keywords
                .push(keyword.clone());
            setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);
            assert_eq!(
                assess_combat_impact(
                    &state,
                    current_incarnation(&state, attacker),
                    current_incarnation(&state, blocker),
                ),
                CombatImpact::Indeterminate,
                "{keyword:?} changes damage timing, assignment, or survival semantics"
            );
        }

        let mut shield_state = setup();
        let shield_attacker = create_creature(&mut shield_state, PlayerId(0), "Attacker", 3, 3);
        let shield_blocker = create_creature(&mut shield_state, PlayerId(1), "Blocker", 2, 2);
        shield_state
            .objects
            .get_mut(&shield_blocker)
            .unwrap()
            .counters
            .insert(CounterType::Shield, 1);
        setup_combat(
            &mut shield_state,
            vec![shield_attacker],
            vec![(shield_attacker, vec![shield_blocker])],
        );
        assert_eq!(
            assess_combat_impact(
                &shield_state,
                current_incarnation(&shield_state, shield_attacker),
                current_incarnation(&shield_state, shield_blocker),
            ),
            CombatImpact::Indeterminate,
            "a shield counter replaces combat damage"
        );

        let mut regeneration_state = setup();
        let regeneration_attacker =
            create_creature(&mut regeneration_state, PlayerId(0), "Attacker", 3, 3);
        let regeneration_blocker =
            create_creature(&mut regeneration_state, PlayerId(1), "Blocker", 2, 2);
        regeneration_state
            .objects
            .get_mut(&regeneration_blocker)
            .unwrap()
            .replacement_definitions
            .push(ReplacementDefinition::new(ReplacementEvent::Destroy).regeneration_shield());
        setup_combat(
            &mut regeneration_state,
            vec![regeneration_attacker],
            vec![(regeneration_attacker, vec![regeneration_blocker])],
        );
        assert_eq!(
            assess_combat_impact(
                &regeneration_state,
                current_incarnation(&regeneration_state, regeneration_attacker),
                current_incarnation(&regeneration_state, regeneration_blocker),
            ),
            CombatImpact::Indeterminate,
            "a regeneration shield changes post-damage survival"
        );

        let mut assignment_state = setup();
        let assignment_attacker =
            create_creature(&mut assignment_state, PlayerId(0), "Attacker", 3, 3);
        let assignment_blocker =
            create_creature(&mut assignment_state, PlayerId(1), "Blocker", 2, 2);
        setup_combat(
            &mut assignment_state,
            vec![assignment_attacker],
            vec![(assignment_attacker, vec![assignment_blocker])],
        );
        assignment_state.combat.as_mut().unwrap().damage_step_index = Some(0);
        assert_eq!(
            assess_combat_impact(
                &assignment_state,
                current_incarnation(&assignment_state, assignment_attacker),
                current_incarnation(&assignment_state, assignment_blocker),
            ),
            CombatImpact::Indeterminate,
            "an in-progress assignment cannot be reassessed as a fresh exchange"
        );

        let mut resilience_state = setup();
        let resilience_attacker =
            create_creature(&mut resilience_state, PlayerId(0), "Attacker", 3, 3);
        let resilience_blocker =
            create_creature(&mut resilience_state, PlayerId(1), "Blocker", 2, 2);
        resilience_state
            .objects
            .get_mut(&resilience_blocker)
            .unwrap()
            .keywords
            .push(Keyword::Indestructible);
        setup_combat(
            &mut resilience_state,
            vec![resilience_attacker],
            vec![(resilience_attacker, vec![resilience_blocker])],
        );
        assert_eq!(
            assess_combat_impact(
                &resilience_state,
                current_incarnation(&resilience_state, resilience_attacker),
                current_incarnation(&resilience_state, resilience_blocker),
            ),
            CombatImpact::Indeterminate,
            "indestructibility separates fixed damage from fixed survival"
        );
    }

    fn add_wolf_subtype(state: &mut GameState, id: ObjectId) {
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .subtypes
            .push("Wolf".to_string());
    }

    fn grant_bands_with_other_wolves(state: &mut GameState, id: ObjectId) {
        add_wolf_subtype(state, id);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .keywords
            .push(Keyword::BandsWithOther("Wolf".to_string()));
    }

    /// Drive `resolve_combat_damage` and, whenever it pauses on an
    /// `AssignCombatDamage` prompt (now reachable for single-blocker trample with
    /// excess per CR 702.19b, as well as the existing 2+ blocker case), submit the
    /// canonical greedy split through the real apply path: lethal to each blocker
    /// in order, then trample the remainder to the attack target (splitting
    /// loyalty-worth to the PW and the rest to its controller for trample-over-PW).
    /// This mirrors `run_combat` in the integration harness and the AI default, so
    /// tests that previously relied on auto-assignment keep asserting that split.
    fn resolve_combat_with_greedy_assignment(state: &mut GameState, events: &mut Vec<GameEvent>) {
        if let Some(waiting) = resolve_combat_damage(state, events) {
            state.waiting_for = waiting;
        } else {
            return;
        }
        while let WaitingFor::AssignCombatDamage {
            attacker_id,
            total_damage,
            ref blockers,
            ref trample,
            pw_loyalty,
            ref attack_target,
            ..
        } = state.waiting_for
        {
            let _ = attacker_id;
            let mut remaining = total_damage;
            let mut assignments: Vec<(ObjectId, u32)> = Vec::new();
            for slot in blockers {
                let assign = remaining.min(slot.lethal_minimum);
                assignments.push((slot.blocker_id, assign));
                remaining = remaining.saturating_sub(assign);
            }
            if trample.is_none() && remaining > 0 {
                if let Some(last) = assignments.last_mut() {
                    last.1 += remaining;
                    remaining = 0;
                }
            }
            let (trample_damage, controller_damage) = if *trample
                == Some(TrampleKind::OverPlaneswalkers)
                && matches!(attack_target, AttackTarget::Planeswalker(_))
            {
                let loyalty = pw_loyalty.unwrap_or(0);
                let to_pw = remaining.min(loyalty);
                (to_pw, remaining.saturating_sub(to_pw))
            } else {
                (if trample.is_some() { remaining } else { 0 }, 0)
            };
            // The answering apply runs the damage batch inline, so its events
            // (damage, lifelink gains, `DamagePrevented` aggregates, rider
            // follow-ups) would be dropped with the `ActionResult`. Collect
            // them so event assertions observe the whole resolution.
            let result = crate::game::engine::apply_as_current(
                state,
                crate::types::actions::GameAction::AssignCombatDamage {
                    mode: CombatDamageAssignmentMode::Normal,
                    assignments,
                    trample_damage,
                    controller_damage,
                },
            )
            .expect("greedy combat damage assignment must be legal");
            events.extend(result.events);
        }
    }

    #[test]
    fn unblocked_attacker_deals_damage_to_player() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.players[1].life, 18); // 20 - 2
    }

    #[test]
    fn blocked_attacker_deals_damage_to_blocker() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Attacker dealt 2 to blocker
        assert_eq!(state.objects[&blocker].damage_marked, 2);
        // Blocker dealt 0 to attacker
        assert_eq!(state.objects[&attacker].damage_marked, 0);
        // No player damage
        assert_eq!(state.players[1].life, 20);
    }

    /// Marks `blocker` as a blocking creature with no attacker assigned (its
    /// attackers were removed from combat, CR 509.1g).
    fn add_orphaned_blocker(state: &mut GameState, blocker: ObjectId) {
        state
            .combat
            .as_mut()
            .unwrap()
            .blocker_to_attacker
            .insert(blocker, vec![]);
    }

    fn combat_damage_sources(events: &[GameEvent]) -> Vec<ObjectId> {
        events
            .iter()
            .filter_map(|event| match event {
                GameEvent::DamageDealt {
                    source_id,
                    is_combat: true,
                    ..
                } => Some(*source_id),
                _ => None,
            })
            .collect()
    }

    /// CR 510.1d: a blocking creature that isn't blocking any creature assigns
    /// no combat damage — and the damage step does not panic on its empty
    /// attacker list. Hostile rows: first strike, double strike, power 0, and a
    /// normal blocker of another attacker beside it. Fails on revert of the
    /// empty-list guard (index panic on `attacker_ids[0]`).
    #[test]
    fn orphaned_blocker_assigns_no_combat_damage() {
        for keyword in [
            None,
            Some(Keyword::FirstStrike),
            Some(Keyword::DoubleStrike),
        ] {
            for orphan_power in [3, 0] {
                let mut state = setup();
                let unblocked = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
                let blocked = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
                let blocker = create_creature(&mut state, PlayerId(1), "Wall", 1, 4);
                let orphan = create_creature(&mut state, PlayerId(1), "Orphan", orphan_power, 4);
                if let Some(keyword) = &keyword {
                    state
                        .objects
                        .get_mut(&orphan)
                        .unwrap()
                        .keywords
                        .push(keyword.clone());
                }
                setup_combat(
                    &mut state,
                    vec![unblocked, blocked],
                    vec![(blocked, vec![blocker])],
                );
                add_orphaned_blocker(&mut state, orphan);

                let mut events = Vec::new();
                resolve_combat_damage(&mut state, &mut events);

                let row = format!("{keyword:?} power {orphan_power}");
                assert_eq!(
                    state.players[1].life, 18,
                    "{row}: reach guard: the unblocked attacker's damage was dealt"
                );
                assert_eq!(
                    state.objects[&blocked].damage_marked, 1,
                    "{row}: the normal blocker still deals its damage"
                );
                assert!(
                    !combat_damage_sources(&events).contains(&orphan),
                    "{row}: CR 510.1d: the orphaned blocker deals no combat damage"
                );
                assert!(
                    state
                        .combat
                        .as_ref()
                        .is_none_or(|combat| !combat.damage_assignments.contains_key(&orphan)),
                    "{row}: the orphan records no damage assignment"
                );
            }
        }
    }

    /// CR 510.4: an orphaned blocking creature is still a blocking creature, so
    /// its first strike opens the first-strike damage step; without first
    /// strike it does not.
    #[test]
    fn orphaned_first_striker_still_opens_first_strike_step() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let orphan = create_creature(&mut state, PlayerId(1), "Orphan", 1, 1);
        setup_combat(&mut state, vec![attacker], vec![]);
        add_orphaned_blocker(&mut state, orphan);
        let combat = state.combat.clone().unwrap();
        assert!(
            !combat_first_strike_participants(&state, &combat).contains(&orphan),
            "paired: an orphan without first strike is excluded"
        );

        state
            .objects
            .get_mut(&orphan)
            .unwrap()
            .keywords
            .push(Keyword::FirstStrike);
        assert!(combat_first_strike_participants(&state, &combat).contains(&orphan));
    }

    /// CR 510.1d: `participates_in_pending_combat_damage_substep` is false for
    /// a blocking creature with no attacker assigned, true once it blocks one.
    /// Fails on revert of the non-empty-assignment guard.
    #[test]
    fn orphan_does_not_participate_in_pending_damage_substep() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let orphan = create_creature(&mut state, PlayerId(1), "Orphan", 3, 3);
        setup_combat(&mut state, vec![attacker], vec![]);
        add_orphaned_blocker(&mut state, orphan);
        assert!(!participates_in_pending_combat_damage_substep(
            &state, orphan
        ));

        state
            .combat
            .as_mut()
            .unwrap()
            .blocker_to_attacker
            .insert(orphan, vec![attacker]);
        assert!(
            participates_in_pending_combat_damage_substep(&state, orphan),
            "reach guard: the same creature blocking an attacker participates"
        );
    }

    // VALIDATION repro for Discord #766/#767: a creature buffed by a transient
    // +1/+1 (Thoughtweft Lieutenant's ETB pump, Blossoming Defense, etc.) must
    // have lethal combat damage measured against its MODIFIED toughness, not its
    // base toughness. CR 510.1c + CR 704.5g: a creature is destroyed only when
    // damage marked on it is >= its current toughness.
    #[test]
    fn buffed_attacker_survives_lethal_against_base_stat_blocker() {
        use crate::types::ability::{ContinuousModification, Duration, TargetFilter};
        use crate::types::game_state::TransientContinuousEffect;

        let mut state = setup();

        // Attacker: printed 2/2, buffed +1/+1 → effective 3/3.
        let attacker = create_creature(&mut state, PlayerId(0), "Kithkin", 2, 2);
        {
            let obj = state.objects.get_mut(&attacker).unwrap();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }
        let ts = state.next_timestamp();
        state
            .transient_continuous_effects
            .push_back(TransientContinuousEffect {
                id: 1,
                source_id: attacker,
                controller: PlayerId(0),
                timestamp: ts,
                duration: Duration::UntilEndOfTurn,
                affected: TargetFilter::SelfRef,
                affected_recipient: None,
                modifications: vec![
                    ContinuousModification::AddPower { value: 1 },
                    ContinuousModification::AddToughness { value: 1 },
                ],
                condition: None,
                duration_subject: None,
                end_permission: None,
                granting_object: None,
                duration_event_source: None,
                source_name: String::new(),
                bound_actor: None,
            });

        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&attacker].toughness,
            Some(3),
            "buff must apply: 2/2 + 1/+1 = 3/3"
        );

        // Blocker equal to the attacker's BASE stats (2/2).
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert_eq!(
            state.objects[&attacker].damage_marked, 2,
            "blocker deals 2 to attacker"
        );

        sba::check_state_based_actions(&mut state, &mut events);

        // The buffed 3/3 took only 2 damage → it must SURVIVE.
        assert_eq!(
            state.objects[&attacker].zone,
            Zone::Battlefield,
            "buffed 3/3 must survive 2 damage from a 2/2 blocker (#766/#767)"
        );
    }

    #[test]
    fn blocked_attacker_with_unblocked_option_waits_for_assignment_choice() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Thorn Elemental", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .assigns_damage_as_though_unblocked = true;
        let blocker_a = create_creature(&mut state, PlayerId(1), "Wall A", 2, 2);
        let blocker_b = create_creature(&mut state, PlayerId(1), "Wall B", 2, 2);
        setup_combat(
            &mut state,
            vec![attacker],
            vec![(attacker, vec![blocker_a, blocker_b])],
        );

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events);

        match waiting {
            Some(WaitingFor::AssignCombatDamage {
                total_damage,
                blockers,
                assignment_modes,
                ..
            }) => {
                assert_eq!(total_damage, 5);
                assert_eq!(blockers.len(), 2);
                assert_eq!(
                    assignment_modes,
                    vec![
                        CombatDamageAssignmentMode::Normal,
                        CombatDamageAssignmentMode::AsThoughUnblocked,
                    ]
                );
            }
            other => panic!("Expected AssignCombatDamage choice, got {other:?}"),
        }
    }

    #[test]
    fn single_blocker_with_unblocked_option_waits_for_assignment_choice() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Thorn Elemental", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .assigns_damage_as_though_unblocked = true;
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events);

        // CR 510.1c: Single blocker would normally auto-assign, but the
        // assigns-damage-as-though-unblocked flag forces interactive choice.
        match waiting {
            Some(WaitingFor::AssignCombatDamage {
                total_damage,
                blockers,
                assignment_modes,
                ..
            }) => {
                assert_eq!(total_damage, 5);
                assert_eq!(blockers.len(), 1);
                assert_eq!(
                    assignment_modes,
                    vec![
                        CombatDamageAssignmentMode::Normal,
                        CombatDamageAssignmentMode::AsThoughUnblocked,
                    ]
                );
            }
            other => panic!("Expected AssignCombatDamage choice, got {other:?}"),
        }
    }

    /// CR 119.3 + CR 510.2: three attackers connecting with a player in one batch
    /// emit one `LifeChanged` each, and each reports the total that hit left the
    /// player on — 18, 16, 14 rather than three copies of the batch's final 14.
    /// A client animating the hits one at a time reads these totals in order, so a
    /// batch that reported only the end state would make every hit show 14.
    #[test]
    fn player_combat_damage_reports_the_total_after_each_hit() {
        let mut state = setup();
        let assignments: Vec<(ObjectId, DamageAssignment)> = (0..3)
            .map(|index| {
                let attacker =
                    create_creature(&mut state, PlayerId(0), &format!("Monk {index}"), 2, 2);
                (
                    attacker,
                    DamageAssignment {
                        target: DamageTarget::Player(PlayerId(1)),
                        amount: 2,
                    },
                )
            })
            .collect();

        let CombatDamageBatch::Complete(events) =
            apply_combat_damage(&mut state, &assignments, CombatDamageSubStep::Regular)
        else {
            panic!("batch must complete: this scenario has no competing life-gain replacement");
        };

        let readings: Vec<(i32, Option<i32>)> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::LifeChanged {
                    player_id,
                    amount,
                    new_total,
                } if *player_id == PlayerId(1) => Some((*amount, new_total.0)),
                _ => None,
            })
            .collect();

        assert_eq!(
            readings,
            vec![(-2, Some(18)), (-2, Some(16)), (-2, Some(14))]
        );
        assert_eq!(state.players[1].life, 14);
    }

    /// CR 122.1c + CR 510.2: a single shield counter prevents ALL combat damage
    /// dealt to the permanent in one simultaneous batch and is removed exactly
    /// once, even when multiple sources deal damage to it.
    #[test]
    fn shield_counter_prevents_all_simultaneous_combat_damage_once() {
        let mut state = setup();
        let shielded = create_creature(&mut state, PlayerId(1), "Shielded Bear", 2, 2);
        state
            .objects
            .get_mut(&shielded)
            .unwrap()
            .counters
            .insert(CounterType::Shield, 1);
        let atk1 = create_creature(&mut state, PlayerId(0), "Attacker A", 3, 3);
        let atk2 = create_creature(&mut state, PlayerId(0), "Attacker B", 3, 3);

        let assignments = vec![
            (
                atk1,
                DamageAssignment {
                    target: DamageTarget::Object(shielded),
                    amount: 3,
                },
            ),
            (
                atk2,
                DamageAssignment {
                    target: DamageTarget::Object(shielded),
                    amount: 3,
                },
            ),
        ];
        let CombatDamageBatch::Complete(events) =
            apply_combat_damage(&mut state, &assignments, CombatDamageSubStep::Regular)
        else {
            panic!("batch must complete: this scenario has no competing life-gain replacement");
        };

        assert_eq!(
            state.objects[&shielded].damage_marked, 0,
            "all simultaneous combat damage must be prevented"
        );
        assert_eq!(
            state.objects[&shielded].counters.get(&CounterType::Shield),
            None,
            "exactly one shield counter consumed for the whole batch"
        );
        let removed = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::CounterRemoved {
                        counter_type: CounterType::Shield,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(removed, 1, "shield counter removed exactly once");
        let prevented = events
            .iter()
            .filter(|e| matches!(e, GameEvent::DamagePrevented { .. }))
            .count();
        assert_eq!(
            prevented, 1,
            "one aggregate DamagePrevented event emitted for the simultaneous batch"
        );
    }

    #[test]
    fn mutual_combat_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.objects[&attacker].damage_marked, 2);
        assert_eq!(state.objects[&blocker].damage_marked, 2);
    }

    #[test]
    fn first_strike_kills_before_regular_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Knight", 3, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::FirstStrike);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // First strike dealt 3 damage (lethal) to blocker
        // SBAs ran between steps -- blocker should have been destroyed
        // Blocker can't deal damage back in regular step (dead)
        // Attacker should have 0 damage
        assert_eq!(state.objects[&attacker].damage_marked, 0);
        // Blocker should be in graveyard (SBAs ran between steps)
        assert!(!state.battlefield.contains(&blocker));
    }

    #[test]
    fn double_strike_deals_damage_twice() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Knight", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::DoubleStrike);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // 3 + 3 = 6 damage to player
        assert_eq!(state.players[1].life, 14);
    }

    #[test]
    fn regular_substep_uses_first_step_keyword_snapshot() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Knight", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::FirstStrike);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 3);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);
        let snapshot = combat_first_strike_participants(&state, state.combat.as_ref().unwrap());
        assert!(snapshot.contains(&attacker));
        assert!(!snapshot.contains(&blocker));

        let combat = state.combat.as_mut().unwrap();
        combat.first_strike_participants = Some(snapshot);
        combat.first_strike_done = true;
        state.objects.get_mut(&attacker).unwrap().keywords.clear();
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .keywords
            .push(Keyword::FirstStrike);

        assert!(
            !participates_in_pending_combat_damage_substep(&state, attacker),
            "losing first strike does not grant a second damage assignment"
        );
        assert!(
            participates_in_pending_combat_damage_substep(&state, blocker),
            "gaining first strike does not remove a normal combatant from regular damage"
        );
    }

    #[test]
    fn trample_assigns_lethal_then_excess_to_player() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Fatty", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        // CR 702.19b: 5/5 trample vs a single 2/2 has excess → interactive prompt
        // (the controller may keep the excess on the blocker or trample it through).
        let waiting = resolve_combat_damage(&mut state, &mut events)
            .expect("single-blocker trample with excess prompts for assignment");
        state.waiting_for = waiting;
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::Normal,
                assignments: vec![(blocker, 2)],
                trample_damage: 3,
                controller_damage: 0,
            },
        )
        .expect("lethal-to-blocker + trample-through is legal");

        // 2 lethal to blocker (dies via SBA), 3 trample excess to player.
        assert!(!state.battlefield.contains(&blocker));
        assert_eq!(state.players[1].life, 17);
    }

    #[test]
    fn trample_deathtouch_assigns_one_to_each_blocker() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "DT Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Deathtouch);
        let blocker1 = create_creature(&mut state, PlayerId(1), "Bear1", 2, 2);
        let blocker2 = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);
        setup_combat(
            &mut state,
            vec![attacker],
            vec![(attacker, vec![blocker1, blocker2])],
        );

        let mut events = Vec::new();
        // 2+ blockers → returns WaitingFor::AssignCombatDamage.
        let waiting = resolve_combat_damage(&mut state, &mut events);
        assert!(matches!(
            waiting,
            Some(WaitingFor::AssignCombatDamage { .. })
        ));

        // Submit: 1 to each blocker (deathtouch lethal), 3 trample to player.
        if let Some(combat) = &mut state.combat {
            combat.pending_damage.push((
                attacker,
                DamageAssignment {
                    target: DamageTarget::Object(blocker1),
                    amount: 1,
                },
            ));
            combat.pending_damage.push((
                attacker,
                DamageAssignment {
                    target: DamageTarget::Object(blocker2),
                    amount: 1,
                },
            ));
            combat.pending_damage.push((
                attacker,
                DamageAssignment {
                    target: DamageTarget::Player(PlayerId(1)),
                    amount: 3,
                },
            ));
            combat.damage_step_index = Some(combat.damage_step_index.unwrap_or(0) + 1);
        }
        let result = resolve_combat_damage(&mut state, &mut events);
        assert!(result.is_none(), "All damage should be resolved");

        // With deathtouch, 1 to each blocker is lethal; 3 excess tramples to player
        assert_eq!(state.objects[&blocker1].damage_marked, 1);
        assert_eq!(state.objects[&blocker2].damage_marked, 1);
        assert_eq!(state.players[1].life, 17);
    }

    #[test]
    fn lifelink_gains_life_on_combat_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Lifelinker", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // 3 damage to defending player
        assert_eq!(state.players[1].life, 17);
        // 3 life gained by controller
        assert_eq!(state.players[0].life, 23);
    }

    /// CR 702.15b: Damage dealt by a source with lifelink causes that source's
    /// controller to gain that much life — regardless of whether the damage is dealt
    /// to a player or to a blocking creature. Regression test for GH #324: user
    /// reported lifelink did not credit life when the attacker was blocked.
    #[test]
    fn lifelink_gains_life_when_attacker_is_blocked() {
        let mut state = setup();
        // 3/3 attacker with lifelink, 2/2 vanilla blocker.
        let attacker = create_creature(&mut state, PlayerId(0), "Lifelinker", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 702.15b: Controller gains life equal to damage dealt to the blocker (3).
        assert_eq!(
            state.players[0].life, 23,
            "Lifelink attacker should gain life from damage dealt to the blocker"
        );
        // Defending player took no damage (attacker was blocked).
        assert_eq!(state.players[1].life, 20);
        // Blocker took 3 damage (and dies via SBA).
        // Either damage_marked is 3 or it's already in the graveyard.
        let blocker_dead = state
            .objects
            .get(&blocker)
            .map(|o| o.zone != Zone::Battlefield)
            .unwrap_or(true);
        assert!(blocker_dead, "Blocker should have died from 3 damage");
    }

    /// CR 119.3 + CR 702.15b: A single lifelink source that deals combat damage to
    /// several recipients at once (here: lethal to a blocker plus trample to the
    /// player) gains life ONCE for the total — so a "whenever you gain life"
    /// trigger (e.g. Blech, Loafing Pest, issue #4366) fires once, not once per
    /// damaged target. Regression: per-assignment life gain emitted one
    /// `LifeChanged` per target, over-triggering such abilities.
    #[test]
    fn lifelink_one_source_many_targets_gains_life_once() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Lifelinker", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        // One source, two simultaneous damage assignments (2 to blocker + 3 to player).
        let assignments = vec![
            (
                attacker,
                DamageAssignment {
                    target: DamageTarget::Object(blocker),
                    amount: 2,
                },
            ),
            (
                attacker,
                DamageAssignment {
                    target: DamageTarget::Player(PlayerId(1)),
                    amount: 3,
                },
            ),
        ];
        let CombatDamageBatch::Complete(events) =
            apply_combat_damage(&mut state, &assignments, CombatDamageSubStep::Regular)
        else {
            panic!("batch must complete: this scenario has no competing life-gain replacement");
        };

        // Exactly one life-gain event for the controller — the whole point.
        let gains: Vec<i32> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::LifeChanged {
                    player_id, amount, ..
                } if *player_id == PlayerId(0) && *amount > 0 => Some(*amount),
                _ => None,
            })
            .collect();
        assert_eq!(
            gains,
            vec![5],
            "one lifelink source must gain life once for the batch total (2+3), \
             not once per damaged target"
        );
        // And the total life gained is still correct (20 + 5).
        assert_eq!(state.players[0].life, 25);
    }

    /// Install a `GainLife` replacement on a new permanent controlled by
    /// `player`. `Multiply{2}` and `Offset{+1}` do not commute (2(n+1) != 2n+1),
    /// so two of them on one player make the CR 616.1 ordering material.
    fn install_gain_life_replacement(
        state: &mut GameState,
        player: PlayerId,
        name: &str,
        amount: QuantityExpr,
    ) -> ObjectId {
        let host = create_creature(state, player, name, 1, 1);
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::GainLife).execute(
                    AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Spell,
                        Effect::GainLife {
                            amount,
                            player: TargetFilter::Controller,
                        },
                    ),
                ),
            );
        host
    }

    fn doubler_amount() -> QuantityExpr {
        QuantityExpr::Multiply {
            factor: 2,
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            }),
        }
    }

    fn plus_one_amount() -> QuantityExpr {
        QuantityExpr::Offset {
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            }),
            offset: 1,
        }
    }

    /// U1 — CR 616.1 + CR 702.15b: a combat lifelink gain facing two
    /// non-commuting life-gain replacements parks the batch instead of dropping
    /// the gain. Derived from the user-reported turn-22 board (Rhox Faithmender
    /// + Leyline of Hope / Cleric Class L2).
    ///
    /// REVERT PROBE: reinstate the old `waiting_before` rollback loop and this
    /// test goes red on every assertion below — at base `resolve_combat_damage`
    /// returns `None`, no record is parked and P0's life stays 20 forever.
    #[test]
    fn lifelink_parks_batch_when_life_gain_ordering_is_material() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Lifelinker", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        install_gain_life_replacement(
            &mut state,
            PlayerId(0),
            "Rhox Faithmender",
            doubler_amount(),
        );
        install_gain_life_replacement(
            &mut state,
            PlayerId(0),
            "Leyline of Hope",
            plus_one_amount(),
        );
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events);

        assert!(
            matches!(waiting, Some(WaitingFor::ReplacementChoice { .. })),
            "CR 616.1: the gaining player must be asked which replacement applies \
             first; got {waiting:?}"
        );
        assert!(
            state.pending_combat_lifelink.is_some(),
            "the unfinished batch tail must be parked, not discarded"
        );
        assert!(
            state.pending_replacement.is_some(),
            "CR 616.1: the paused source's own LifeGain is the resume's authority"
        );
        assert_eq!(
            state.players[1].life, 17,
            "CR 510.2: the damage is already dealt — only the gain is deferred"
        );
        assert_eq!(
            state.players[0].life, 20,
            "no life is gained until the ordering choice is answered"
        );
        assert!(
            !state.combat.as_ref().unwrap().regular_damage_done,
            "CR 510.4: the sub-step is not complete while the batch is parked"
        );
    }

    /// U2 — the positive reach-guard and negative sibling for U1: with ONE
    /// life-gain replacement the ordering is not material (CR 616.1), so the
    /// gain resolves inline with no prompt and no parked record.
    #[test]
    fn lifelink_single_life_gain_replacement_applies_without_prompt() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Lifelinker", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        install_gain_life_replacement(
            &mut state,
            PlayerId(0),
            "Rhox Faithmender",
            doubler_amount(),
        );
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events);

        assert!(
            !matches!(waiting, Some(WaitingFor::ReplacementChoice { .. })),
            "one applicable replacement needs no CR 616.1 choice; got {waiting:?}"
        );
        assert!(
            state.pending_combat_lifelink.is_none(),
            "a batch that never pauses parks nothing"
        );
        assert_eq!(
            state.players[0].life, 26,
            "CR 614.6: the doubled gain (3 -> 6) is applied inline"
        );
    }

    /// U3 — CR 510.2 + CR 615.5: the completing batch's event ORDER. The
    /// lifelink `LifeChanged` precedes the per-player `CombatDamageDealtToPlayer`
    /// aggregate, which precedes Phase D's `DamagePrevented` rider. Nothing in
    /// the repo pinned this before, and the frontend's lifelink fold
    /// (`eventNormalizer.ts`) closes its window one index before the aggregate.
    ///
    /// REVERT PROBE: move `drain_combat_lifelink` after
    /// `fire_batch_prevention_riders` and the first two indices invert.
    #[test]
    fn completed_batch_emits_gain_then_aggregate_then_prevention_rider() {
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));
        let prevented = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
        let lifelinker = create_creature(&mut state, PlayerId(0), "Vampire", 3, 3);
        state
            .objects
            .get_mut(&lifelinker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(
            &mut state,
            vec![prevented, lifelinker],
            vec![(prevented, vec![]), (lifelinker, vec![blocker])],
        );

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        let gain = events
            .iter()
            .position(|e| {
                matches!(e, GameEvent::LifeChanged { player_id, amount, .. }
                    if *player_id == PlayerId(0) && *amount > 0)
            })
            .expect("the lifelink gain is emitted");
        let aggregate = events
            .iter()
            .position(|e| matches!(e, GameEvent::CombatDamageDealtToPlayer { .. }))
            .expect("the per-player aggregate is emitted");
        let prevented_at = events
            .iter()
            .position(|e| matches!(e, GameEvent::DamagePrevented { .. }))
            .expect("Phase D emits the batch's prevention event");
        assert!(
            gain < aggregate,
            "CR 702.15b: the lifelink gain is one of the damage event's own \
             results and precedes the aggregate (gain={gain}, aggregate={aggregate})"
        );
        assert!(
            aggregate < prevented_at,
            "CR 615.5: the prevention rider takes place immediately AFTER the \
             event it replaced (aggregate={aggregate}, prevented={prevented_at})"
        );
    }

    #[test]
    fn combat_no_combat_state_is_noop() {
        let mut state = setup();
        state.combat = None;
        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert!(events.is_empty());
    }

    #[test]
    fn multiple_blockers_returns_waiting_for_assignment() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Fatty", 5, 5);
        let blocker1 = create_creature(&mut state, PlayerId(1), "Bear1", 2, 2);
        let blocker2 = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);
        setup_combat(
            &mut state,
            vec![attacker],
            vec![(attacker, vec![blocker1, blocker2])],
        );

        let mut events = Vec::new();
        // CR 510.1c: 2+ blockers → interactive assignment required.
        let waiting = resolve_combat_damage(&mut state, &mut events);
        match &waiting {
            Some(WaitingFor::AssignCombatDamage {
                total_damage,
                blockers,
                trample,
                ..
            }) => {
                assert_eq!(*total_damage, 5);
                assert_eq!(blockers.len(), 2);
                assert!(trample.is_none());
            }
            other => panic!("Expected AssignCombatDamage, got {:?}", other),
        }

        // Submit: free division — all 5 to blocker1, 0 to blocker2 (legal under current rules).
        if let Some(combat) = &mut state.combat {
            combat.pending_damage.push((
                attacker,
                DamageAssignment {
                    target: DamageTarget::Object(blocker1),
                    amount: 5,
                },
            ));
            combat.damage_step_index = Some(combat.damage_step_index.unwrap_or(0) + 1);
        }
        let result = resolve_combat_damage(&mut state, &mut events);
        assert!(result.is_none(), "All damage should be resolved");

        // All 5 to blocker1, none to blocker2
        assert_eq!(state.objects[&blocker1].damage_marked, 5);
        assert_eq!(state.objects[&blocker2].damage_marked, 0);
        // No damage to player
        assert_eq!(state.players[1].life, 20);
    }

    #[test]
    fn bands_with_other_blocker_reroutes_attacker_damage_assignment() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Attacker", 5, 5);
        let blocker1 = create_creature(&mut state, PlayerId(1), "Wolf Blocker", 2, 2);
        let blocker2 = create_creature(&mut state, PlayerId(1), "Plain Wolf Blocker", 2, 2);
        grant_bands_with_other_wolves(&mut state, blocker1);
        add_wolf_subtype(&mut state, blocker2);
        setup_combat(
            &mut state,
            vec![attacker],
            vec![(attacker, vec![blocker1, blocker2])],
        );

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events);
        match waiting {
            Some(WaitingFor::AssignCombatDamage { player, .. }) => {
                assert_eq!(
                    player,
                    PlayerId(1),
                    "CR 702.22j: defending player assigns attacker damage"
                );
            }
            other => panic!("expected AssignCombatDamage, got {other:?}"),
        }
    }

    #[test]
    fn bands_with_other_attackers_reroute_blocker_damage_assignment() {
        let mut state = setup();
        let attacker1 = create_creature(&mut state, PlayerId(0), "Wolf A", 2, 2);
        let attacker2 = create_creature(&mut state, PlayerId(0), "Wolf B", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Guard", 3, 3);
        grant_bands_with_other_wolves(&mut state, attacker1);
        add_wolf_subtype(&mut state, attacker2);
        setup_combat(
            &mut state,
            vec![attacker1, attacker2],
            vec![(attacker1, vec![blocker]), (attacker2, vec![blocker])],
        );

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events);
        match waiting {
            Some(WaitingFor::AssignBlockerDamage { player, .. }) => {
                assert_eq!(
                    player,
                    PlayerId(0),
                    "CR 702.22k: active player assigns blocker damage"
                );
            }
            other => panic!("expected AssignBlockerDamage, got {other:?}"),
        }
    }

    #[test]
    fn deathtouch_marks_flag_on_target() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "DT", 1, 1);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Deathtouch);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert!(state.objects[&blocker].dealt_deathtouch_damage);
    }

    #[test]
    fn wither_applies_minus_counters_to_creature_instead_of_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Wither", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Wither);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 4);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Wither: 3 -1/-1 counters instead of damage_marked
        assert_eq!(state.objects[&blocker].damage_marked, 0);
        assert_eq!(
            state.objects[&blocker]
                .counters
                .get(&crate::types::counter::CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0),
            3
        );
    }

    #[test]
    fn wither_to_player_deals_normal_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Wither", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Wither);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Wither does NOT give poison to players, just normal damage
        assert_eq!(state.players[1].life, 17);
        assert_eq!(state.players[1].poison_counters, 0);
    }

    #[test]
    fn infect_applies_minus_counters_to_creature() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Infector", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Infect);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 4);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Infect: -1/-1 counters on creature
        assert_eq!(state.objects[&blocker].damage_marked, 0);
        assert_eq!(
            state.objects[&blocker]
                .counters
                .get(&crate::types::counter::CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0),
            3
        );
    }

    #[test]
    fn infect_to_player_gives_poison_no_life_loss() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Infector", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Infect);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Infect: poison counters, no life loss
        assert_eq!(state.players[1].life, 20);
        assert_eq!(state.players[1].poison_counters, 3);
    }

    #[test]
    fn toxic_to_player_adds_poison_and_still_deals_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Toxic", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Toxic(2));
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.players[1].life, 17);
        assert_eq!(state.players[1].poison_counters, 2);
    }

    /// CR 702.164b + CR 702.164c (issue #955): a creature with a printed Toxic 1
    /// plus a granted Toxic 1 has total toxic value 2, so combat damage to a
    /// player gives 2 poison counters. This is the end-to-end combat consequence
    /// of the layer-6 summing fix; pre-fix the granted Toxic was deduped away and
    /// the player received only 1 poison counter.
    #[test]
    fn printed_plus_granted_toxic_gives_two_poison_in_combat() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Toxic", 3, 3);
        let obj = state.objects.get_mut(&attacker).unwrap();
        // Two distinct Toxic(1) instances (printed + granted) coexisting on the
        // keyword list — exactly the state the layer-6 summing fix produces.
        obj.keywords.push(Keyword::Toxic(1));
        obj.keywords.push(Keyword::Toxic(1));
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.players[1].life, 17, "3 combat damage still dealt");
        assert_eq!(
            state.players[1].poison_counters, 2,
            "CR 702.164c: total toxic value (1+1) yields 2 poison counters"
        );
    }

    #[test]
    fn toxic_to_creature_does_not_add_poison() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Toxic", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Toxic(2));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 4);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.objects[&blocker].damage_marked, 3);
        assert_eq!(state.players[1].poison_counters, 0);
    }

    #[test]
    fn lifelink_works_with_infect() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "InfectLinker", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Infect);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Infect gives poison, but lifelink still triggers
        assert_eq!(state.players[1].poison_counters, 3);
        assert_eq!(state.players[0].life, 23); // gained 3 life
    }

    #[test]
    fn commander_damage_tracked_when_commander_hits_player() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Commander", 5, 5);
        state.objects.get_mut(&attacker).unwrap().is_commander = true;
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Commander dealt 5 damage to player 1
        assert_eq!(state.players[1].life, 15);
        // Commander damage tracked
        assert_eq!(state.commander_damage.len(), 1);
        assert_eq!(state.commander_damage[0].player, PlayerId(1));
        assert_eq!(state.commander_damage[0].commander, attacker);
        assert_eq!(state.commander_damage[0].damage, 5);
    }

    #[test]
    fn commander_damage_accumulates_over_multiple_combats() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Commander", 3, 3);
        state.objects.get_mut(&attacker).unwrap().is_commander = true;
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert_eq!(state.commander_damage[0].damage, 3);

        // Second combat
        state.combat = None;
        state.objects.get_mut(&attacker).unwrap().tapped = false;
        setup_combat(&mut state, vec![attacker], vec![]);
        events.clear();
        resolve_combat_damage(&mut state, &mut events);

        // Accumulated: 3 + 3 = 6
        assert_eq!(state.commander_damage[0].damage, 6);
    }

    #[test]
    fn non_commander_damage_not_tracked() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        // is_commander defaults to false
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.players[1].life, 18);
        assert!(state.commander_damage.is_empty());
    }

    #[test]
    fn different_commanders_tracked_separately() {
        let mut state = setup();
        let cmd_a = create_creature(&mut state, PlayerId(0), "Cmd A", 3, 3);
        state.objects.get_mut(&cmd_a).unwrap().is_commander = true;
        let cmd_b = create_creature(&mut state, PlayerId(0), "Cmd B", 2, 2);
        state.objects.get_mut(&cmd_b).unwrap().is_commander = true;
        setup_combat(&mut state, vec![cmd_a, cmd_b], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Two separate entries
        assert_eq!(state.commander_damage.len(), 2);
        let entry_a = state
            .commander_damage
            .iter()
            .find(|e| e.commander == cmd_a)
            .unwrap();
        let entry_b = state
            .commander_damage
            .iter()
            .find(|e| e.commander == cmd_b)
            .unwrap();
        assert_eq!(entry_a.damage, 3);
        assert_eq!(entry_b.damage, 2);
    }

    #[test]
    fn one_or_more_combat_damage_trigger_fires_once_per_damage_step() {
        let mut state = setup();
        let watcher = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Professional Face-Breaker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&watcher)
            .unwrap()
            .trigger_definitions
            .push({
                let mut trigger = TriggerDefinition::new(TriggerMode::DamageDoneOnceByController)
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Spell,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: crate::types::ability::TargetFilter::Controller,
                        },
                    ));
                trigger.valid_source = Some(crate::types::ability::TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                ));
                trigger.valid_target = Some(crate::types::ability::TargetFilter::Player);
                trigger
            });

        let attacker_a = create_creature(&mut state, PlayerId(0), "Attacker A", 2, 2);
        let attacker_b = create_creature(&mut state, PlayerId(0), "Attacker B", 3, 3);
        setup_combat(&mut state, vec![attacker_a, attacker_b], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.stack.len(), 1);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                GameEvent::CombatDamageDealtToPlayer {
                    player_id,
                    source_amounts,
                    ..
                } if *player_id == PlayerId(1)
                    && source_amounts.len() == 2
                    && source_amounts.iter().any(|(id, _)| *id == attacker_a)
                    && source_amounts.iter().any(|(id, _)| *id == attacker_b)
            )
        }));
    }

    #[test]
    fn one_or_more_combat_damage_trigger_tracks_matching_sources_for_those_creatures() {
        let mut state = setup();
        let watcher = create_object(
            &mut state,
            CardId(550),
            PlayerId(0),
            "Heroes in a Half Shell".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&watcher)
            .unwrap()
            .trigger_definitions
            .push({
                let valid_source = TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(
                            TypedFilter::default()
                                .subtype("Mutant".to_string())
                                .controller(ControllerRef::You),
                        ),
                        TargetFilter::Typed(
                            TypedFilter::default()
                                .subtype("Ninja".to_string())
                                .controller(ControllerRef::You),
                        ),
                        TargetFilter::Typed(
                            TypedFilter::default()
                                .subtype("Turtle".to_string())
                                .controller(ControllerRef::You),
                        ),
                    ],
                };
                let mut trigger = TriggerDefinition::new(TriggerMode::DamageDoneOnceByController)
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Spell,
                        Effect::PutCounterAll {
                            counter_type: CounterType::Plus1Plus1,
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::TrackedSet {
                                id: TrackedSetId(0),
                            },
                        },
                    ));
                trigger.valid_source = Some(valid_source);
                trigger.valid_target = Some(TargetFilter::Player);
                trigger
            });

        let mutant = create_creature(&mut state, PlayerId(0), "Mutant", 2, 2);
        state
            .objects
            .get_mut(&mutant)
            .unwrap()
            .card_types
            .subtypes
            .push("Mutant".to_string());
        let human = create_creature(&mut state, PlayerId(0), "Human", 2, 2);
        state
            .objects
            .get_mut(&human)
            .unwrap()
            .card_types
            .subtypes
            .push("Human".to_string());
        state
            .tracked_object_sets
            .insert(TrackedSetId(99), vec![human]);
        setup_combat(&mut state, vec![mutant, human], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert_eq!(state.stack.len(), 1);

        crate::game::stack::resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&mutant]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or_default(),
            1
        );
        assert_eq!(
            state.objects[&human]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or_default(),
            0
        );
    }

    #[test]
    fn one_or_more_combat_damage_trigger_fires_in_each_double_strike_step() {
        let mut state = setup();
        let watcher = create_object(
            &mut state,
            CardId(600),
            PlayerId(0),
            "Damage Watcher".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&watcher)
            .unwrap()
            .trigger_definitions
            .push({
                let mut trigger = TriggerDefinition::new(TriggerMode::DamageDoneOnceByController)
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Spell,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: crate::types::ability::TargetFilter::Controller,
                        },
                    ));
                trigger.valid_source = Some(crate::types::ability::TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                ));
                trigger.valid_target = Some(crate::types::ability::TargetFilter::Player);
                trigger
            });

        let attacker = create_creature(&mut state, PlayerId(0), "Double Striker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::DoubleStrike);
        setup_combat(&mut state, vec![attacker], vec![]);

        // Stock P0's library so the per-step draw trigger never draws from empty.
        for _ in 0..2 {
            let card_id = CardId(state.next_object_id);
            create_object(
                &mut state,
                card_id,
                PlayerId(0),
                "Lib".to_string(),
                Zone::Library,
            );
        }

        let mut events = Vec::new();
        // CR 510.4: First-strike sub-step. The double striker deals its 2 damage,
        // the DamageDone trigger fires (stack len 1), and CR 510.3 grants priority
        // before the regular sub-step — so resolve_combat_damage pauses here.
        let waiting = resolve_combat_damage(&mut state, &mut events);
        assert!(
            matches!(waiting, Some(WaitingFor::Priority { .. })),
            "CR 510.3: priority is granted after the first-strike sub-step's trigger is stacked"
        );
        assert_eq!(
            state.stack.len(),
            1,
            "CR 510.3a: only the first-strike sub-step's trigger is on the stack so far"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::CombatDamageDealtToPlayer { .. }))
                .count(),
            1,
            "CR 510.4: the double striker dealt damage once in the first-strike sub-step"
        );

        // CR 510.3 + CR 510.4: resolve the first-strike trigger, then re-enter the
        // turn-based action for the mandatory regular (second) combat-damage sub-step.
        crate::game::stack::resolve_top(&mut state, &mut events);
        assert!(state.stack.is_empty(), "first-strike trigger resolved");
        resolve_combat_damage(&mut state, &mut events);

        // CR 510.4: The double striker deals damage AGAIN in the regular sub-step,
        // so the DamageDone trigger fires a second time (now on the stack).
        assert_eq!(state.stack.len(), 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::CombatDamageDealtToPlayer { .. }))
                .count(),
            2,
            "CR 510.4: the double striker dealt combat damage in both sub-steps"
        );
    }

    /// Regression test: lifelink life gain during combat damage can activate a conditional
    /// static +2/+2 ability, increasing toughness before SBAs check for lethal damage.
    /// This validates the composition of three building blocks:
    /// - CR 702.15b: Lifelink life gain simultaneous with damage
    /// - CR 604.2: Static abilities create continuous effects while on the battlefield
    /// - CR 704.3: SBAs re-evaluate layers before checking lethal damage
    #[test]
    fn lifelink_conditional_static_saves_from_lethal() {
        let mut state = setup();
        state.format_config.starting_life = 20;
        // Player 0 at 26 life — one lifelink hit (gaining 2) pushes past 27 threshold.
        state.players[0].life = 26;

        // Attacker A: 3/3 — blocked by 3/3. Takes 3 damage (lethal without buff, survives with +2/+2).
        let attacker_a = create_creature(&mut state, PlayerId(0), "Tank", 3, 3);
        // Attacker B: 2/2 with lifelink — unblocked, gains 2 life for controller.
        let attacker_b = create_creature(&mut state, PlayerId(0), "Lifelinker", 2, 2);
        state
            .objects
            .get_mut(&attacker_b)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        // Blocker: 3/3 blocking Attacker A.
        let blocker = create_creature(&mut state, PlayerId(1), "Blocker", 3, 3);

        // Enchantment with conditional static: "if life >= starting + 7, creatures you control get +2/+2"
        let ench_card_id = CardId(state.next_object_id);
        let ench_id = create_object(
            &mut state,
            ench_card_id,
            PlayerId(0),
            "Life Anthem".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ench_id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        let static_def = StaticDefinition::continuous()
            .affected(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .into(),
            )
            .condition(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeAboveStarting,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 7 },
            })
            .modifications(vec![
                ContinuousModification::AddPower { value: 2 },
                ContinuousModification::AddToughness { value: 2 },
            ]);
        obj.static_definitions.push(static_def.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(static_def);

        setup_combat(
            &mut state,
            vec![attacker_a, attacker_b],
            vec![(attacker_a, vec![blocker])],
        );
        state.layers_dirty.mark_full();

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 702.15b: Lifelink gained 2 life (26 → 28).
        assert_eq!(state.players[0].life, 28);

        // CR 604.2 + CR 704.3: Static +2/+2 activated before SBA lethal check.
        // Attacker A survived — toughness 5 (3 base + 2 static), damage was only 3.
        assert!(
            state.battlefield.contains(&attacker_a),
            "Attacker A should survive: toughness 5 (3+2) > 3 damage"
        );
        assert_eq!(state.objects[&attacker_a].damage_marked, 3);

        // Blocker died — took 3 damage on 3 toughness (attacker dealt 3 at assignment time).
        assert!(
            !state.battlefield.contains(&blocker),
            "Blocker should be destroyed: 3 damage >= 3 toughness"
        );

        // Defending player took 2 from unblocked lifelinker.
        assert_eq!(state.players[1].life, 18);
    }

    fn create_planeswalker(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        loyalty: u32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        // CR 306.5b: loyalty field and counter map mirror each other.
        obj.loyalty = Some(loyalty);
        obj.counters
            .insert(crate::types::counter::CounterType::Loyalty, loyalty);
        id
    }

    // CR 510.1b: Unblocked creature attacking a planeswalker deals damage to the PW, not the player.
    #[test]
    fn unblocked_attacker_damages_planeswalker_not_player() {
        use crate::game::combat::AttackTarget;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Grizzly Bears", 2, 2);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Test Planeswalker", 4);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        });

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // PW should have lost 2 loyalty (4 → 2), player life unchanged
        let pw_obj = state.objects.get(&pw).unwrap();
        assert_eq!(
            pw_obj.loyalty,
            Some(2),
            "PW should have 2 loyalty after 2 damage"
        );
        assert_eq!(state.players[1].life, 20, "Player life should be unchanged");
    }

    // CR 702.19f: Regular trample excess goes to the PW, not the defending player.
    #[test]
    fn trample_excess_goes_to_planeswalker_not_player() {
        use crate::game::combat::AttackTarget;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Big Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Small Blocker", 1, 2);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Test Planeswalker", 6);

        let mut combat = CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        };
        // Assign blocker
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        if let Some(info) = combat
            .attackers
            .iter_mut()
            .find(|a| a.object_id == attacker)
        {
            info.blocked = true;
        }
        state.combat = Some(combat);

        let mut events = Vec::new();
        resolve_combat_with_greedy_assignment(&mut state, &mut events);

        // Blocker has 2 toughness: 2 damage lethal, 3 excess to PW (not player)
        let pw_obj = state.objects.get(&pw).unwrap();
        assert_eq!(
            pw_obj.loyalty,
            Some(3),
            "PW should have 3 loyalty (6 - 3 trample excess)"
        );
        assert_eq!(
            state.players[1].life, 20,
            "Player life should be unchanged — CR 702.19f"
        );
    }

    // CR 506.4c: If the PW leaves the battlefield before damage, attacker deals no damage.
    #[test]
    fn planeswalker_leaves_before_damage_no_damage_dealt() {
        use crate::game::combat::AttackTarget;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Grizzly Bears", 2, 2);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Doomed Planeswalker", 3);
        let pw_attack_target = AttackTarget::Planeswalker(pw);

        // Set up combat with attacker targeting the PW
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(attacker, pw_attack_target, PlayerId(1))],
            ..Default::default()
        });

        // Remove the PW from battlefield before damage
        if let Some(obj) = state.objects.get_mut(&pw) {
            obj.zone = Zone::Graveyard;
        }
        state.battlefield.retain(|&id| id != pw);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 506.4c: No damage to player OR planeswalker
        assert_eq!(
            state.players[1].life, 20,
            "Player should take no damage when PW left"
        );
    }

    // ── Trample Over Planeswalkers (CR 702.19c) ────────────────────────────

    // CR 702.19c: Single blocker + PW target + trample-over-PW → splits blocker/PW/controller.
    #[test]
    fn trample_over_pw_single_blocker_splits_damage() {
        use crate::game::combat::AttackTarget;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        // 7/7 with trample over planeswalkers
        let attacker = create_creature(&mut state, PlayerId(0), "Big Trampler", 7, 7);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::TrampleOverPlaneswalkers);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Jace", 3);

        let mut combat = CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        };
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        combat
            .attackers
            .iter_mut()
            .find(|a| a.object_id == attacker)
            .unwrap()
            .blocked = true;
        state.combat = Some(combat);

        let mut events = Vec::new();
        resolve_combat_with_greedy_assignment(&mut state, &mut events);

        // 7 power: 2 lethal to blocker, 3 to PW (loyalty), 2 to PW controller
        assert_eq!(
            state.objects[&pw].loyalty,
            Some(0),
            "PW should have 0 loyalty (3 - 3)"
        );
        assert_eq!(
            state.players[1].life, 18,
            "Player should take 2 damage (7 - 2 blocker - 3 PW loyalty)"
        );
    }

    // CR 702.19f preserved: regular trample excess stays on PW, not controller.
    #[test]
    fn regular_trample_excess_stays_on_pw_not_controller() {
        use crate::game::combat::AttackTarget;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 7, 7);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Jace", 3);

        let mut combat = CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        };
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        combat
            .attackers
            .iter_mut()
            .find(|a| a.object_id == attacker)
            .unwrap()
            .blocked = true;
        state.combat = Some(combat);

        let mut events = Vec::new();
        resolve_combat_with_greedy_assignment(&mut state, &mut events);

        // CR 702.19f: All 5 excess (7 - 2 lethal) goes to PW, not player
        assert_eq!(
            state.objects[&pw].loyalty,
            Some(0),
            "PW should lose all loyalty to excess"
        );
        assert_eq!(
            state.players[1].life, 20,
            "Player should take NO damage — CR 702.19f"
        );
    }

    // CR 702.19e: PW removed + trample-over-PW → damage redirects to defending player.
    #[test]
    fn trample_over_pw_redirects_when_pw_removed() {
        use crate::game::combat::AttackTarget;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::TrampleOverPlaneswalkers);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Doomed PW", 4);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        });

        // Remove PW before damage
        state.objects.get_mut(&pw).unwrap().zone = Zone::Graveyard;
        state.battlefield.retain(|&id| id != pw);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 702.19e: All damage to defending player
        assert_eq!(
            state.players[1].life, 15,
            "5 damage should redirect to defending player — CR 702.19e"
        );
    }

    // CR 702.19b: Trample-over-PW attacking a player behaves like standard trample.
    #[test]
    fn trample_over_pw_attacking_player_behaves_as_standard() {
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::TrampleOverPlaneswalkers);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_with_greedy_assignment(&mut state, &mut events);

        // 5 power: 2 lethal to blocker, 3 trample to player (same as standard trample)
        assert_eq!(
            state.players[1].life, 17,
            "3 trample damage to player — CR 702.19b"
        );
    }

    // CR 702.19d: Trample + blocked but no blockers remaining → damage to attack target.
    #[test]
    fn trample_blocked_no_blockers_damages_attack_target() {
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 4, 4);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        // Set up combat with blocker, then remove the blocker
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);
        // Remove blocker from the assignment list (simulating it left before damage)
        if let Some(c) = &mut state.combat {
            c.blocker_assignments.insert(attacker, vec![]);
        }

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 702.19d: All damage to defending player
        assert_eq!(
            state.players[1].life, 16,
            "4 trample damage to player — CR 702.19d"
        );
    }

    // CR 702.19d + 702.19c: Trample-over-PW + blocked but no blockers + attacking PW.
    #[test]
    fn trample_over_pw_blocked_no_blockers_splits_pw_controller() {
        use crate::game::combat::AttackTarget;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::TrampleOverPlaneswalkers);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Jace", 3);

        let mut combat = CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        };
        // Blocker was assigned but then removed
        combat.blocker_assignments.insert(attacker, vec![]);
        combat
            .attackers
            .iter_mut()
            .find(|a| a.object_id == attacker)
            .unwrap()
            .blocked = true;
        state.combat = Some(combat);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 702.19d + 702.19c: 3 to PW (loyalty), 2 to controller
        assert_eq!(
            state.objects[&pw].loyalty,
            Some(0),
            "PW should have 0 loyalty"
        );
        assert_eq!(
            state.players[1].life, 18,
            "Player should take 2 damage (5 - 3 PW loyalty)"
        );
    }

    // CR 702.19c + CR 702.2c: Deathtouch + trample-over-PW maximizes spillover.
    #[test]
    fn deathtouch_trample_over_pw_maximizes_spillover() {
        use crate::game::combat::AttackTarget;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "DT Trampler", 6, 6);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::TrampleOverPlaneswalkers);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Deathtouch);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Jace", 3);

        let mut combat = CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        };
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        combat
            .attackers
            .iter_mut()
            .find(|a| a.object_id == attacker)
            .unwrap()
            .blocked = true;
        state.combat = Some(combat);

        let mut events = Vec::new();
        resolve_combat_with_greedy_assignment(&mut state, &mut events);

        // 6 power: 1 deathtouch lethal to blocker, 3 to PW (loyalty), 2 to controller
        assert_eq!(
            state.objects[&pw].loyalty,
            Some(0),
            "PW should have 0 loyalty"
        );
        assert_eq!(
            state.players[1].life, 18,
            "Player should take 2 damage (6 - 1 deathtouch - 3 PW loyalty)"
        );
    }

    // Keyword FromStr round-trip.
    #[test]
    fn keyword_from_str_trample_over_planeswalkers() {
        use crate::types::keywords::Keyword;
        let kw: Keyword = "trample over planeswalkers".parse().unwrap();
        assert_eq!(kw, Keyword::TrampleOverPlaneswalkers);
        // "trample" must still parse to regular Trample
        let kw2: Keyword = "trample".parse().unwrap();
        assert_eq!(kw2, Keyword::Trample);
    }

    // --- #314: combat-damage prevention batch + Inkshield rider aggregation ---

    /// Resolve an Inkshield-style spell ("prevent all combat damage to `player`
    /// this turn; for each 1 damage prevented this way, create a 2/1 Inkling")
    /// through the real `PreventDamage` effect resolver. The shield lands in
    /// `pending_damage_replacements` with its `runtime_execute` Token rider.
    fn install_inkshield(state: &mut GameState, player: PlayerId) {
        use crate::game::effects::prevent_damage;
        use crate::types::ability::{
            PreventionAmount, PreventionScope, PtValue, ResolvedAbility, TargetFilter,
        };
        use crate::types::mana::ManaColor;

        let shield_source = create_object(
            state,
            CardId(state.next_object_id),
            player,
            "Inkshield".to_string(),
            Zone::Stack,
        );

        let mut token = ResolvedAbility::new(
            Effect::Token {
                name: "Inkling".to_string(),
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string(), "Inkling".to_string()],
                colors: vec![ManaColor::White, ManaColor::Black],
                keywords: vec![Keyword::Flying],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            shield_source,
            player,
        );
        token.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        });

        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Controller,
                recipient_scope: crate::types::ability::EffectScope::Single,
                scope: PreventionScope::CombatDamage,
                damage_source_filter: None,
                prevention_duration: None,
            },
            vec![],
            shield_source,
            player,
        )
        .sub_ability(token);

        let mut events = Vec::new();
        prevent_damage::resolve(state, &ability, &mut events).unwrap();
    }

    fn count_inklings(state: &GameState) -> usize {
        state
            .objects
            .values()
            .filter(|obj| obj.zone == Zone::Battlefield && obj.name == "Inkling")
            .count()
    }

    /// Step 1: Two unblocked 3/3 attackers hit an Inkshield controller. The
    /// rider must fire once against the aggregate (6), creating 6 Inklings —
    /// not 3+3 from two separate firings, and not 0 from a fragmented count.
    #[test]
    fn test_inkshield_aggregates_combat_damage_into_tokens() {
        let mut state = setup();
        // PlayerId(1) controls Inkshield; PlayerId(0) attacks them.
        install_inkshield(&mut state, PlayerId(1));

        let a1 = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
        let a2 = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
        setup_combat(&mut state, vec![a1, a2], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 615.6: prevented damage never happens — 0 life lost.
        assert_eq!(state.players[1].life, 20, "all 6 combat damage prevented");
        // CR 615.7 + CR 615.13: one rider firing against the aggregate of 6.
        assert_eq!(
            count_inklings(&state),
            6,
            "rider fires once for the whole batch: 3 + 3 = 6 Inklings"
        );
    }

    /// Step 6: A double-strike attacker vs Inkshield. CR 510.4 — first-strike
    /// and regular damage are separate combat-damage steps, each its own
    /// simultaneous batch → its own rider firing. A 4/4 double-striker
    /// prevents 4 then 4: two firings, 8 Inklings total.
    #[test]
    fn test_inkshield_double_strike_fires_rider_per_combat_step() {
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));

        let attacker = create_creature(&mut state, PlayerId(0), "Striker", 4, 4);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::DoubleStrike);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.players[1].life, 20, "both strikes prevented");
        // CR 510.4: two separate combat-damage steps → two rider firings of 4.
        assert_eq!(
            count_inklings(&state),
            8,
            "double strike: 4 (first-strike step) + 4 (regular step) = 8"
        );
        // CR 510.4: two DamagePrevented events, one per combat-damage step.
        let prevented = events
            .iter()
            .filter(|e| matches!(e, GameEvent::DamagePrevented { .. }))
            .count();
        assert_eq!(prevented, 2, "one DamagePrevented per combat-damage step");
    }

    /// Step 7: Trample attacker partially blocked, defending player shielded.
    /// The trample-to-player damage batches with the to-creature assignment;
    /// the shield aggregates only the player-targeted portion (CR 615.7).
    #[test]
    fn test_inkshield_trample_aggregates_only_player_portion() {
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));

        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_with_greedy_assignment(&mut state, &mut events);

        // 2 lethal to the blocker, 3 trample over to the shielded player.
        // The 0/2 Wall takes its lethal portion and dies via SBA (CR 704.5g) —
        // to-creature damage is not prevented by the player-scoped shield.
        assert!(
            !state.battlefield.contains(&blocker),
            "blocker takes its lethal portion (not prevented) and dies"
        );
        assert_eq!(state.players[1].life, 20, "trample-over damage prevented");
        // CR 615.7: only the 3 player-targeted damage is aggregated.
        assert_eq!(count_inklings(&state), 3, "3 trample damage → 3 Inklings");
    }

    /// CR 615.13 vs CR 120.3 at the seam — the row that pins the split.
    ///
    /// One shield covering BOTH legs of one CR 510.2 simultaneous batch (the
    /// installed Inkshield's `damage_target_filter` is nulled here, which is the
    /// shape of every Fog-class / `PreventDamage{Any}` shield — 218 of them in the
    /// corpus — and of Safe Passage's two-legged filter): a trample attacker puts
    /// 2 on the blocker and 3 over to the player.
    ///
    /// CR 120.3 makes the RECIPIENT per-event, so TWO `DamagePrevented` events are
    /// emitted, one per recipient, each with its own amount. CR 615.13 makes the
    /// APPLICATION the trigger/rider unit, so the CR 615.5 rider fires ONCE — five
    /// Inklings from the batch total, not ten from firing per recipient.
    ///
    /// This is the single most likely mis-implementation of the per-recipient
    /// split, and nothing else in the suite catches it: every other prevention
    /// fixture is single-recipient, so per-shield and per-recipient firing
    /// coincide.
    #[test]
    fn multi_recipient_shield_emits_per_recipient_but_fires_its_rider_once() {
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));
        // Widen the resolution-created shield to cover creatures too. Installed
        // floating (no host permanent), so it lives under the ObjectId(0) sentinel
        // in `pending_damage_replacements`.
        state
            .pending_damage_replacements
            .iter_mut()
            .for_each(|repl| repl.damage_target_filter = None);

        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_with_greedy_assignment(&mut state, &mut events);

        // Reach-guards: BOTH legs really were prevented. The 0/2 Wall surviving a
        // 5/5 trampler is the creature leg's proof; life 20 is the player leg's.
        assert!(
            state.battlefield.contains(&blocker),
            "reach-guard: the widened shield prevented the blocker's lethal 2, so \
             CR 704.5g never removed it"
        );
        assert_eq!(
            state.objects[&blocker].damage_marked, 0,
            "reach-guard: the creature leg was prevented, not merely survived"
        );
        assert_eq!(
            state.players[1].life, 20,
            "reach-guard: the 3 trample-over damage was prevented"
        );

        // CR 120.3: one event per RECIPIENT, each carrying that recipient's own
        // amount. Exact list, length included.
        let mut prevented: Vec<(TargetRef, u32)> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::DamagePrevented { target, amount, .. } => {
                    Some((target.clone(), *amount))
                }
                _ => None,
            })
            .collect();
        prevented.sort();
        let mut expected = vec![
            (TargetRef::Object(blocker), 2),
            (TargetRef::Player(PlayerId(1)), 3),
        ];
        expected.sort();
        assert_eq!(
            prevented, expected,
            "CR 120.3: two recipients in one application → two events, 2 and 3"
        );

        // CR 615.13 + CR 615.5: ONE rider firing against the batch TOTAL. Ten
        // Inklings would mean the rider fired once per recipient.
        assert_eq!(
            count_inklings(&state),
            5,
            "the CR 615.5 rider fires ONCE per prevention application, against the \
             group total (2 + 3) — not once per recipient"
        );
    }

    /// Step 8: A mixed batch — attacker X hits a creature, attacker Y hits the
    /// shielded player, in one `apply_combat_damage` call. The shield
    /// aggregates only Y's amount (CR 615.7).
    #[test]
    fn test_inkshield_mixed_creature_and_player_batch() {
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));

        let x = create_creature(&mut state, PlayerId(0), "Ogre X", 3, 3);
        let y = create_creature(&mut state, PlayerId(0), "Ogre Y", 4, 4);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 6);
        setup_combat(
            &mut state,
            vec![x, y],
            vec![(x, vec![blocker]), (y, vec![])],
        );

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(
            state.objects[&blocker].damage_marked, 3,
            "X's 3 to-creature damage applies normally"
        );
        assert_eq!(
            state.players[1].life, 20,
            "Y's 4 to-player damage prevented"
        );
        assert_eq!(count_inklings(&state), 4, "only Y's 4 damage → 4 Inklings");
    }

    /// Step 9: A deathtouch attacker in a batch alongside a prevented attacker.
    /// The Phase A/B/C split must not disturb deathtouch marking — the
    /// deathtouch creature still dies to SBA.
    #[test]
    fn test_deathtouch_unaffected_by_combat_damage_batch_split() {
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));

        // Deathtouch attacker vs a fat blocker; prevented attacker hits player.
        let dt = create_creature(&mut state, PlayerId(0), "Adder", 1, 1);
        state
            .objects
            .get_mut(&dt)
            .unwrap()
            .keywords
            .push(Keyword::Deathtouch);
        let blocker = create_creature(&mut state, PlayerId(1), "Giant", 6, 6);
        let unblocked = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
        setup_combat(
            &mut state,
            vec![dt, unblocked],
            vec![(dt, vec![blocker]), (unblocked, vec![])],
        );

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 702.2c: 1 deathtouch damage is lethal — the 6/6 blocker dies to SBA.
        assert!(
            state
                .objects
                .get(&blocker)
                .is_none_or(|o| o.zone != Zone::Battlefield),
            "deathtouch creature destroys the blocker via SBA"
        );
        // The prevented attacker still made 3 Inklings.
        assert_eq!(state.players[1].life, 20);
        assert_eq!(count_inklings(&state), 3);
    }

    /// Step 10: Lifelink (CR 615.6 / CR 702.15b). A fully prevented lifelink
    /// attacker gains 0 life; a non-prevented lifelink attacker in the same
    /// batch still gains life.
    #[test]
    fn test_lifelink_prevented_gains_no_life_unprevented_still_gains() {
        // Case A: lifelink attacker fully prevented → 0 life gained.
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));
        let ll = create_creature(&mut state, PlayerId(0), "Vampire", 3, 3);
        state
            .objects
            .get_mut(&ll)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        setup_combat(&mut state, vec![ll], vec![]);
        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert_eq!(
            state.players[0].life, 20,
            "CR 615.6: prevented damage never happens — no lifelink"
        );

        // Case B: lifelink attacker NOT prevented, batched alongside a
        // prevented attacker → lifelink still fires.
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));
        let prevented = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
        let pw_target = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let ll = create_creature(&mut state, PlayerId(0), "Vampire", 3, 3);
        state
            .objects
            .get_mut(&ll)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        // lifelink attacker hits a creature (not the shielded player).
        setup_combat(
            &mut state,
            vec![prevented, ll],
            vec![(prevented, vec![]), (ll, vec![pw_target])],
        );
        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert_eq!(
            state.players[0].life, 23,
            "lifelink attacker's 3 to-creature damage gains 3 life"
        );
        assert_eq!(state.players[1].life, 20, "the other attacker is prevented");
    }

    /// Step 11: Commander damage bookkeeping. A commander attacker prevented +
    /// a commander attacker dealing damage in one batch — `commander_damage`
    /// accrues only the unprevented commander (CR 704.6c).
    #[test]
    fn test_commander_damage_accrues_only_unprevented_commander() {
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));

        let prevented_cmdr = create_creature(&mut state, PlayerId(0), "Cmdr A", 4, 4);
        state.objects.get_mut(&prevented_cmdr).unwrap().is_commander = true;
        let dealing_cmdr = create_creature(&mut state, PlayerId(0), "Cmdr B", 5, 5);
        state.objects.get_mut(&dealing_cmdr).unwrap().is_commander = true;

        // Both attack PlayerId(1). The shield prevents ALL combat damage to
        // PlayerId(1), so commander damage must accrue 0 for both.
        setup_combat(&mut state, vec![prevented_cmdr, dealing_cmdr], vec![]);
        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 704.6c: prevented commander damage does not accrue.
        let total: u32 = state
            .commander_damage
            .iter()
            .filter(|e| e.player == PlayerId(1))
            .map(|e| e.damage)
            .sum();
        assert_eq!(total, 0, "fully prevented — no commander damage accrues");

        // Now an unblocked commander with the shield gone: damage accrues.
        let mut state = setup();
        let cmdr = create_creature(&mut state, PlayerId(0), "Cmdr", 5, 5);
        state.objects.get_mut(&cmdr).unwrap().is_commander = true;
        setup_combat(&mut state, vec![cmdr], vec![]);
        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        let entry = state
            .commander_damage
            .iter()
            .find(|e| e.player == PlayerId(1) && e.commander == cmdr);
        assert_eq!(
            entry.map(|e| e.damage),
            Some(5),
            "unprevented commander accrues 5 combat damage"
        );
    }

    // === CR 702.19b: single-blocker trample interactive-assignment truth table ===
    //
    // Build a REAL combat state with the blocker present in state.objects so
    // lethal_damage_needed reads its real toughness, then exercise
    // needs_interactive_assignment directly.
    fn needs_interactive(
        power: i32,
        blocker_toughness: i32,
        trample: Option<TrampleKind>,
        deathtouch: bool,
        extra_blocker_toughness: Option<i32>,
    ) -> bool {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Attacker", power, power);
        if let Some(kind) = trample {
            let kw = match kind {
                TrampleKind::Standard => Keyword::Trample,
                TrampleKind::OverPlaneswalkers => Keyword::TrampleOverPlaneswalkers,
            };
            state.objects.get_mut(&attacker).unwrap().keywords.push(kw);
        }
        if deathtouch {
            state
                .objects
                .get_mut(&attacker)
                .unwrap()
                .keywords
                .push(Keyword::Deathtouch);
        }
        let blocker = create_creature(&mut state, PlayerId(1), "Blocker", 1, blocker_toughness);
        let mut blockers = vec![blocker];
        if let Some(t) = extra_blocker_toughness {
            let blocker2 = create_creature(&mut state, PlayerId(1), "Blocker2", 1, t);
            blockers.push(blocker2);
        }
        setup_combat(&mut state, vec![attacker], vec![(attacker, blockers)]);

        let combat = state.combat.as_ref().unwrap().clone();
        let attacker_info = combat
            .attackers
            .iter()
            .find(|a| a.object_id == attacker)
            .unwrap()
            .clone();
        let obj = state.objects.get(&attacker).unwrap().clone();
        needs_interactive_assignment(
            &state,
            &obj,
            &combat,
            &attacker_info,
            power.max(0) as u32,
            deathtouch,
            trample,
        )
    }

    #[test]
    fn single_blocker_trample_with_excess_needs_interactive() {
        // CR 702.19b: 5/5 trample vs 2/2 → 3 excess → controller chooses.
        assert!(needs_interactive(
            5,
            2,
            Some(TrampleKind::Standard),
            false,
            None
        ));
    }

    #[test]
    fn single_blocker_trample_no_excess_auto_assigns() {
        // CR 510.1c: 2/2 trample vs 2/2 → power == lethal → no choice.
        assert!(!needs_interactive(
            2,
            2,
            Some(TrampleKind::Standard),
            false,
            None
        ));
    }

    #[test]
    fn single_blocker_no_trample_auto_assigns() {
        // CR 510.1c: single blocker without trample → all damage to blocker.
        assert!(!needs_interactive(5, 2, None, false, None));
    }

    #[test]
    fn two_blockers_need_interactive() {
        // CR 510.1c: 2+ blockers → controller divides.
        assert!(needs_interactive(5, 2, None, false, Some(2)));
    }

    #[test]
    fn single_blocker_trample_deathtouch_with_excess_needs_interactive() {
        // CR 702.2c + CR 702.19b: deathtouch makes lethal 1; power 5 > 1 → excess.
        assert!(needs_interactive(
            5,
            2,
            Some(TrampleKind::Standard),
            true,
            None
        ));
    }

    #[test]
    fn single_blocker_trample_deathtouch_no_excess_auto_assigns() {
        // CR 702.2c: deathtouch lethal is 1; power 1 == lethal → no choice.
        assert!(!needs_interactive(
            1,
            2,
            Some(TrampleKind::Standard),
            true,
            None
        ));
    }

    // === CR 702.19b: single-blocker trample runtime, driven through the pipeline ===

    #[test]
    fn single_blocker_trample_excess_prompts_assignment() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Fatty", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events);

        // CR 702.19b: single-blocker trample with excess → interactive prompt.
        match waiting {
            Some(WaitingFor::AssignCombatDamage {
                ref blockers,
                trample,
                total_damage,
                ..
            }) => {
                assert_eq!(blockers.len(), 1);
                assert_eq!(blockers[0].lethal_minimum, 2);
                assert_eq!(trample, Some(TrampleKind::Standard));
                assert_eq!(total_damage, 5);
            }
            other => panic!("Expected AssignCombatDamage, got {other:?}"),
        }
    }

    #[test]
    fn single_blocker_trample_outcome_a_trample_through() {
        // Outcome A: keep lethal on blocker, trample excess to player.
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Fatty", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events)
            .expect("single-blocker trample with excess must prompt");
        // A bare return from resolve_combat_damage does not write state.waiting_for;
        // the apply path requires it, so set it from the returned WaitingFor.
        state.waiting_for = waiting;

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::Normal,
                assignments: vec![(blocker, 2)],
                trample_damage: 3,
                controller_damage: 0,
            },
        )
        .expect("trample-through assignment is legal");

        // CR 702.19b: 2 lethal to blocker (it dies via SBA → off battlefield, so
        // damage_marked is cleared on the zone change), 3 excess tramples to the
        // defending player. Player life is the discriminator vs Outcome B.
        assert!(
            !state.battlefield.contains(&blocker),
            "blocker took lethal (2) and should have died"
        );
        assert_eq!(state.players[1].life, 17);
    }

    #[test]
    fn trample_multi_blocker_power_below_combined_lethal_no_excess_is_legal() {
        // CR 702.19b + CR 510.1c: a 4/4 trample attacker blocked by two 3/3s has
        // combined lethal 6 > power 4, so the controller cannot assign lethal to
        // both AND send excess to the player.  The controller MUST be allowed to
        // freely distribute all 4 damage among the two blockers with trample_damage=0.
        // (The frontend #1491 bug: trampleLethalMet unconditionally required lethal
        // to every blocker, deadlocking combat here.)
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Rampager", 4, 4);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker_a = create_creature(&mut state, PlayerId(1), "Guard A", 3, 3);
        let blocker_b = create_creature(&mut state, PlayerId(1), "Guard B", 3, 3);
        setup_combat(
            &mut state,
            vec![attacker],
            vec![(attacker, vec![blocker_a, blocker_b])],
        );

        let mut events = Vec::new();
        let waiting =
            resolve_combat_damage(&mut state, &mut events).expect("2 blockers must always prompt");
        state.waiting_for = waiting;

        // Assign 2 to each blocker, 0 trample through; this must succeed even though
        // neither blocker gets lethal (CR 702.19b: lethal gating only applies when
        // excess is being sent to the player/PW).
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::Normal,
                assignments: vec![(blocker_a, 2), (blocker_b, 2)],
                trample_damage: 0,
                controller_damage: 0,
            },
        )
        .expect("distributing all damage among blockers with trample_damage=0 is legal");

        // Neither blocker takes lethal; both survive. Defending player untouched.
        assert!(
            state.battlefield.contains(&blocker_a),
            "blocker_a took 2 < lethal (3) and must survive"
        );
        assert!(
            state.battlefield.contains(&blocker_b),
            "blocker_b took 2 < lethal (3) and must survive"
        );
        assert_eq!(state.players[1].life, 20, "no trample damage to player");
    }

    #[test]
    fn single_blocker_trample_outcome_b_keep_on_blocker() {
        // Outcome B (the user's case): controller keeps ALL 5 on the blocker and
        // tramples nothing through. CR 702.19b: assigning no excess to the player
        // is legal (need not assign lethal to blocker before keeping excess there).
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Fatty", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events)
            .expect("single-blocker trample with excess must prompt");
        state.waiting_for = waiting;

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::Normal,
                assignments: vec![(blocker, 5)],
                trample_damage: 0,
                controller_damage: 0,
            },
        )
        .expect("keeping all damage on the blocker is legal (CR 702.19b)");

        // CR 702.19b: all 5 stay on the blocker (it dies via SBA → off battlefield),
        // and NONE tramples through — the defending player is untouched. The
        // unchanged player life (20) is the proof the controller may decline to
        // assign excess to the player (the user's reported case).
        assert!(
            !state.battlefield.contains(&blocker),
            "blocker took 5 (>= lethal) and should have died"
        );
        assert_eq!(state.players[1].life, 20);
    }

    /// CR 701.19a + CR 510.2: Regeneration shield installed before combat
    /// damage must survive lethal damage from the combat-damage SBA pass.
    #[test]
    fn regeneration_shield_survives_combat_damage_resolution() {
        use crate::types::ability::{ReplacementDefinition, TargetFilter};
        use crate::types::replacements::ReplacementEvent;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Lotleth Troll", 2, 1);
        let blocker = create_creature(&mut state, PlayerId(1), "Big Blocker", 5, 5);
        {
            let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
                .valid_card(TargetFilter::SelfRef)
                .description("Regenerate".to_string())
                .regeneration_shield();
            state
                .objects
                .get_mut(&attacker)
                .unwrap()
                .replacement_definitions
                .push(shield);
        }
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(
            state.objects[&attacker].zone,
            Zone::Battlefield,
            "attacker with regen shield must survive lethal combat damage"
        );
        assert_eq!(state.objects[&attacker].damage_marked, 0);
    }

    // ── `combat_damage_to_defender` (CR 510.1 / 702.4b / 702.7b / 702.19b/d) ──
    //
    // The building block itself, exercised over the rule interactions rather than
    // through any one consumer. Each case is a different pair of rules meeting.

    fn kw(state: &mut GameState, id: ObjectId, keywords: Vec<Keyword>) {
        state.objects.get_mut(&id).unwrap().keywords = keywords;
    }

    #[test]
    fn unblocked_attacker_assigns_its_whole_power() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 7, 7);
        // CR 510.1b
        assert_eq!(combat_damage_to_defender(&state, a, &[]), 7);
    }

    #[test]
    fn blocked_nontrampler_assigns_nothing_to_the_player() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 11, 11);
        let b = create_creature(&mut state, PlayerId(1), "B", 1, 1);
        // CR 510.1c: even a 1/1 chump stops all 11.
        assert_eq!(combat_damage_to_defender(&state, a, &[b]), 0);
    }

    #[test]
    fn trampler_assigns_the_excess_over_lethal_to_blockers() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 11, 11);
        kw(&mut state, a, vec![Keyword::Trample]);
        let b1 = create_creature(&mut state, PlayerId(1), "B1", 1, 4);
        let b2 = create_creature(&mut state, PlayerId(1), "B2", 1, 4);
        // CR 702.19b: 4 + 4 lethal, 3 excess.
        assert_eq!(combat_damage_to_defender(&state, a, &[b1, b2]), 3);
    }

    #[test]
    fn trampler_assigns_nothing_when_it_cannot_cover_every_blocker() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 5, 5);
        kw(&mut state, a, vec![Keyword::Trample]);
        let b1 = create_creature(&mut state, PlayerId(1), "B1", 1, 4);
        let b2 = create_creature(&mut state, PlayerId(1), "B2", 1, 4);
        // CR 702.19b: "need not assign lethal damage to all those blocking creatures
        // but in that case can't assign any damage to the player".
        assert_eq!(combat_damage_to_defender(&state, a, &[b1, b2]), 0);
    }

    #[test]
    fn marked_damage_lowers_the_lethal_each_blocker_absorbs() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 11, 11);
        kw(&mut state, a, vec![Keyword::Trample]);
        let b1 = create_creature(&mut state, PlayerId(1), "B1", 1, 4);
        let b2 = create_creature(&mut state, PlayerId(1), "B2", 1, 4);
        state.objects.get_mut(&b1).unwrap().damage_marked = 3;
        state.objects.get_mut(&b2).unwrap().damage_marked = 3;
        // CR 702.19b: "take into account damage already marked" — 1 + 1, not 4 + 4.
        assert_eq!(combat_damage_to_defender(&state, a, &[b1, b2]), 9);
    }

    #[test]
    fn deathtouch_trampler_needs_only_one_damage_per_blocker() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 11, 11);
        kw(&mut state, a, vec![Keyword::Trample, Keyword::Deathtouch]);
        let b1 = create_creature(&mut state, PlayerId(1), "B1", 1, 6);
        let b2 = create_creature(&mut state, PlayerId(1), "B2", 1, 6);
        // CR 702.2c: 1 each is lethal, so 9 tramples through despite 12 toughness.
        assert_eq!(combat_damage_to_defender(&state, a, &[b1, b2]), 9);
    }

    #[test]
    fn double_strike_trampler_strikes_the_empty_block_again() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 11, 11);
        kw(&mut state, a, vec![Keyword::DoubleStrike, Keyword::Trample]);
        let b1 = create_creature(&mut state, PlayerId(1), "B1", 1, 4);
        let b2 = create_creature(&mut state, PlayerId(1), "B2", 1, 4);
        // CR 702.4b: first step assigns 4 + 4 lethal and tramples 3, emptying the
        // block; CR 702.19d: the regular step then assigns all 11 to the player.
        assert_eq!(combat_damage_to_defender(&state, a, &[b1, b2]), 14);
    }

    #[test]
    fn double_strike_without_trample_still_assigns_nothing_to_the_player() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 11, 11);
        kw(&mut state, a, vec![Keyword::DoubleStrike]);
        let b = create_creature(&mut state, PlayerId(1), "B", 1, 1);
        // CR 510.1c: the second strike has no blockers and no trample, so no damage
        // is assigned at all — the distinction CR 702.19d turns on.
        assert_eq!(combat_damage_to_defender(&state, a, &[b]), 0);
    }

    #[test]
    fn first_striking_blocker_that_kills_the_attacker_stops_the_regular_step() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 11, 2);
        kw(&mut state, a, vec![Keyword::Trample]);
        let b = create_creature(&mut state, PlayerId(1), "B", 3, 1);
        kw(&mut state, b, vec![Keyword::FirstStrike]);
        // CR 702.7b: the blocker's 3 kills the 11/2 in the first step, so the
        // attacker never assigns its own damage at all.
        assert_eq!(combat_damage_to_defender(&state, a, &[b]), 0);
    }

    #[test]
    fn first_strike_attacker_kills_blockers_before_they_can_trade() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 11, 11);
        kw(&mut state, a, vec![Keyword::FirstStrike, Keyword::Trample]);
        let b = create_creature(&mut state, PlayerId(1), "B", 1, 4);
        // CR 702.7b: a lone first-strike step — 4 lethal, 7 through. It does NOT
        // strike again (that would need double strike).
        assert_eq!(combat_damage_to_defender(&state, a, &[b]), 7);
    }

    /// CR 702.4b: an unblocked double striker assigns in both damage steps. Pinned
    /// against `resolve_combat_damage`'s own answer — `double_strike_deals_damage_twice`
    /// has an unblocked 3/3 double striker deal 6 — because this helper claims to be
    /// the authority for the same question and must not disagree with the resolver.
    #[test]
    fn unblocked_double_striker_assigns_in_both_steps() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 3, 3);
        kw(&mut state, a, vec![Keyword::DoubleStrike]);
        assert_eq!(combat_damage_to_defender(&state, a, &[]), 6);
    }

    /// Plain first strike is one step, not two — the distinction double strike turns
    /// on. Guards against "strikes first" being conflated with "strikes twice".
    #[test]
    fn unblocked_first_striker_assigns_once() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 3, 3);
        kw(&mut state, a, vec![Keyword::FirstStrike]);
        assert_eq!(combat_damage_to_defender(&state, a, &[]), 3);
    }

    /// CR 702.19b: damage assigned to a surviving blocker stays marked on it, so the
    /// regular step sees a reduced lethal minimum. A 5-power double-strike trampler
    /// into one 0/6 marks 5 in the first step (0 through, since lethal was not
    /// assigned to every blocker), then needs only 1 more in the regular step and
    /// tramples the other 4.
    #[test]
    fn damage_marked_in_the_first_step_lowers_lethal_for_the_second() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 5, 5);
        kw(&mut state, a, vec![Keyword::DoubleStrike, Keyword::Trample]);
        let b = create_creature(&mut state, PlayerId(1), "B", 0, 6);
        assert_eq!(combat_damage_to_defender(&state, a, &[b]), 4);
    }

    #[test]
    fn zero_power_attacker_assigns_no_combat_damage() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 0, 3);
        kw(&mut state, a, vec![Keyword::Trample]);
        // CR 510.1a
        assert_eq!(combat_damage_to_defender(&state, a, &[]), 0);
    }

    // ── Modified damage basis (CR 613.11) ──
    //
    // `combat_damage_amount` is what `resolve_combat_damage` assigns from, so this
    // helper must read the same number. Every case below is chosen to give a
    // different answer if either side is re-derived from raw power.

    /// CR 613.11 + CR 702.19b: a Doran-style attacker assigns its toughness, so a
    /// 1/5 trampler covers a 0/2 blocker's lethal and tramples 3. Read from raw
    /// power it assigns 1, fails to cover the blocker, and gets 0 through.
    #[test]
    fn attacker_assigning_from_toughness_tramples_on_its_toughness() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 1, 5);
        kw(&mut state, a, vec![Keyword::Trample]);
        state
            .objects
            .get_mut(&a)
            .unwrap()
            .assigns_damage_from_toughness = true;
        let b = create_creature(&mut state, PlayerId(1), "B", 0, 2);
        assert_eq!(combat_damage_to_defender(&state, a, &[b]), 3);
    }

    /// CR 510.1a: "assigns no combat damage" zeroes an unblocked attacker that would
    /// otherwise connect for its full power.
    #[test]
    fn attacker_assigning_no_combat_damage_reaches_the_player_for_nothing() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 7, 7);
        kw(&mut state, a, vec![Keyword::Trample]);
        state.objects.get_mut(&a).unwrap().assigns_no_combat_damage = true;
        assert_eq!(combat_damage_to_defender(&state, a, &[]), 0);
    }

    /// CR 613.11 + CR 702.7b: a first-striking blocker assigning from toughness kills
    /// the 11/2 attacker with its 3 toughness before the regular step, so nothing
    /// tramples through. Read from raw power it assigns 0 and 8 gets through.
    #[test]
    fn first_strike_blocker_assigning_from_toughness_kills_the_attacker() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 11, 2);
        kw(&mut state, a, vec![Keyword::Trample]);
        let b = create_creature(&mut state, PlayerId(1), "B", 0, 3);
        kw(&mut state, b, vec![Keyword::FirstStrike]);
        state
            .objects
            .get_mut(&b)
            .unwrap()
            .assigns_damage_from_toughness = true;
        assert_eq!(combat_damage_to_defender(&state, a, &[b]), 0);
    }

    /// CR 510.1a + CR 702.7b: the mirror case — a first-striking blocker that assigns
    /// no combat damage cannot kill the attacker, so the regular step still happens
    /// and 10 tramples past its 1 toughness. Read from raw power its 5 would be
    /// lethal to the 11/2 and this would be 0.
    #[test]
    fn first_strike_blocker_assigning_no_combat_damage_cannot_kill_the_attacker() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 11, 2);
        kw(&mut state, a, vec![Keyword::Trample]);
        let b = create_creature(&mut state, PlayerId(1), "B", 5, 1);
        kw(&mut state, b, vec![Keyword::FirstStrike]);
        state.objects.get_mut(&b).unwrap().assigns_no_combat_damage = true;
        assert_eq!(combat_damage_to_defender(&state, a, &[b]), 10);
    }

    /// Verbatim Oracle text (Scryfall).
    const NO_MERCY: &str = "Whenever a creature deals damage to you, destroy it.";

    /// Drives a scenario at P0's precombat main to its combat damage step:
    /// `attackers` attack P1, and `blocks` (`(blocker, attacker)`) are declared
    /// when P1 is asked. Returns once the phase is `CombatDamage`; each caller
    /// asserts the `waiting_for` it expects there, so a drive that stops short
    /// fails.
    fn drive_to_combat_damage(
        runner: &mut GameRunner,
        attackers: &[ObjectId],
        blocks: Vec<(ObjectId, ObjectId)>,
    ) {
        runner.pass_both_players();
        runner
            .act(GameAction::DeclareAttackers {
                attacks: attackers
                    .iter()
                    .map(|&id| (id, AttackTarget::Player(P1)))
                    .collect(),
                bands: vec![],
            })
            .expect("declare attackers");
        let mut blocks = Some(blocks);
        for _ in 0..8 {
            if runner.state().phase == Phase::CombatDamage {
                return;
            }
            match &runner.state().waiting_for {
                WaitingFor::DeclareBlockers { .. } => {
                    runner
                        .act(GameAction::DeclareBlockers {
                            assignments: blocks.take().unwrap_or_default(),
                        })
                        .expect("declare blockers");
                }
                WaitingFor::Priority { .. } => runner.pass_both_players(),
                _ => return,
            }
        }
    }

    fn combat_damage_steps(runner: &GameRunner) -> u32 {
        runner
            .state()
            .steps_started_this_turn
            .count(Phase::CombatDamage)
    }

    fn combat(runner: &GameRunner) -> &CombatState {
        runner
            .state()
            .combat
            .as_ref()
            .expect("combat is in progress")
    }

    /// CR 510.4 + CR 702.7b + CR 702.4b: a first-strike combat damage step is
    /// followed by a second combat damage step, and both are counted. Here the
    /// second step follows the first in the same `resolve_combat_damage` call.
    #[test]
    fn step_tally_counts_the_second_combat_damage_step() {
        for double_strike in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let attacker = if double_strike {
                // Fencing Ace: 1/1, double strike.
                scenario
                    .add_creature(P0, "Fencing Ace", 1, 1)
                    .double_strike()
                    .id()
            } else {
                // Youthful Knight: 2/1, first strike.
                scenario
                    .add_creature(P0, "Youthful Knight", 2, 1)
                    .first_strike()
                    .id()
            };
            let mut runner = scenario.build();
            let life_before = runner.life(P1);
            drive_to_combat_damage(&mut runner, &[attacker], vec![]);

            let state = runner.state();
            assert!(
                matches!(state.waiting_for, WaitingFor::Priority { .. }),
                "double strike {double_strike}: {:?}",
                state.waiting_for
            );
            assert!(state.stack.is_empty());
            assert!(combat(&runner).first_strike_done && combat(&runner).regular_damage_done);
            // Youthful Knight hits once for 2; Fencing Ace hits in both steps for 1.
            assert_eq!(
                runner.life(P1),
                life_before - 2,
                "double strike {double_strike}"
            );
            assert_eq!(
                state.steps_started_this_turn.count(Phase::BeginCombat),
                1,
                "one combat"
            );
            assert_eq!(
                combat_damage_steps(&runner),
                2,
                "double strike {double_strike}: the first-strike step and the second step"
            );
        }
    }

    /// CR 510.4's paired negative: with neither first strike nor double strike
    /// there is one combat damage step, counted once.
    #[test]
    fn step_tally_counts_one_combat_damage_step_without_first_strike() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let mut runner = scenario.build();
        let life_before = runner.life(P1);
        drive_to_combat_damage(&mut runner, &[bears], vec![]);

        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        // Reach guards: the regular sub-step head was reached and dealt the damage.
        assert!(combat(&runner)
            .first_strike_participants
            .as_ref()
            .is_some_and(|participants| participants.is_empty()));
        assert!(!combat(&runner).first_strike_done);
        assert!(combat(&runner).regular_damage_done);
        assert_eq!(runner.life(P1), life_before - 2);
        assert_eq!(combat_damage_steps(&runner), 1);
    }

    /// CR 510.3a + CR 510.4: a trigger from the first-strike step pauses the
    /// combat before the second step. The second step is counted once, when it
    /// begins through the `priority.rs` completeness gate, and not before.
    #[test]
    fn step_tally_counts_the_second_combat_damage_step_after_a_pause() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let knight = scenario
            .add_creature(P0, "Youthful Knight", 2, 1)
            .first_strike()
            .id();
        scenario
            .add_creature_from_oracle(P1, "No Mercy", 0, 0, NO_MERCY)
            .as_enchantment();
        let mut runner = scenario.build();
        let life_before = runner.life(P1);
        drive_to_combat_damage(&mut runner, &[knight], vec![]);

        // In the pause: No Mercy's trigger is on the stack after the first step.
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(runner.state().stack.len(), 1);
        assert!(combat(&runner).first_strike_done);
        assert!(!combat(&runner).regular_damage_done);
        assert_eq!(runner.life(P1), life_before - 2);
        assert_eq!(
            combat_damage_steps(&runner),
            1,
            "the second step has not begun"
        );

        runner.advance_until_stack_empty();
        for _ in 0..4 {
            if runner.state().phase != Phase::CombatDamage || combat(&runner).regular_damage_done {
                break;
            }
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority into the second combat damage step");
        }

        assert_eq!(runner.state().phase, Phase::CombatDamage);
        assert!(combat(&runner).regular_damage_done);
        assert_eq!(runner.state().objects[&knight].zone, Zone::Graveyard);
        assert_eq!(runner.life(P1), life_before - 2);
        assert_eq!(combat_damage_steps(&runner), 2);
    }

    /// CR 510.1c + CR 510.4: an interactive damage assignment inside the second
    /// combat damage step resumes that step; it does not begin another one.
    #[test]
    fn step_tally_does_not_recount_a_resumed_second_combat_damage_step() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let knight = scenario
            .add_creature(P0, "Youthful Knight", 2, 1)
            .first_strike()
            .id();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let cadet_1 = scenario.add_creature(P1, "Eager Cadet", 1, 1).id();
        let cadet_2 = scenario.add_creature(P1, "Eager Cadet", 1, 1).id();
        let mut runner = scenario.build();
        let life_before = runner.life(P1);
        drive_to_combat_damage(
            &mut runner,
            &[knight, bears],
            vec![(cadet_1, bears), (cadet_2, bears)],
        );

        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::AssignCombatDamage { attacker_id, .. } if attacker_id == bears
            ),
            "{:?}",
            runner.state().waiting_for
        );
        assert!(combat(&runner).first_strike_done);
        assert!(!combat(&runner).regular_damage_done);
        assert!(combat(&runner).damage_step_index.is_some());
        assert_eq!(runner.life(P1), life_before - 2);
        assert_eq!(combat_damage_steps(&runner), 2);

        // Grizzly Bears' 2 power divided as P0 chooses between its two blockers,
        // CR 510.1c; 1 each destroys both 1/1 Cadets.
        runner
            .act(GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::Normal,
                assignments: vec![(cadet_1, 1), (cadet_2, 1)],
                trample_damage: 0,
                controller_damage: 0,
            })
            .expect("assign the Bears' damage");

        assert!(combat(&runner).regular_damage_done);
        assert_eq!(runner.state().objects[&cadet_1].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&cadet_2].zone, Zone::Graveyard);
        assert_eq!(combat_damage_steps(&runner), 2);
    }

    // ── F1: pausable combat-damage batch (CR 616.1 protection overlap) ──

    /// Paint a source's colors so protection qualities match it.
    fn paint(state: &mut GameState, id: ObjectId, colors: Vec<crate::types::mana::ManaColor>) {
        state.objects.get_mut(&id).unwrap().color = colors;
    }

    /// Grant TWO protection qualities (object keywords or player-scope
    /// `PlayerProtection` statics) so combat damage from a source matching
    /// BOTH pauses for a CR 616.1 ordering choice. Player-scope grants are
    /// statics (Serra's-Emissary/Absolute-Virtue class): color qualities are
    /// inert at the player level, and only the `Everything` transient arm
    /// reads the transient table.
    fn install_protection_overlap(
        state: &mut GameState,
        scope: TargetRef,
        first: crate::types::keywords::ProtectionTarget,
        second: crate::types::keywords::ProtectionTarget,
    ) {
        use crate::types::keywords::Keyword;
        match scope {
            TargetRef::Object(id) => {
                state
                    .objects
                    .get_mut(&id)
                    .unwrap()
                    .keywords
                    .extend([Keyword::Protection(first), Keyword::Protection(second)]);
            }
            TargetRef::Player(player) => {
                use crate::types::ability::ChosenAttribute;
                use crate::types::statics::StaticMode;
                let grantor = create_creature(state, player, "Shelter", 1, 1);
                // `ChosenCardType` grants (Serra's-Emissary class) resolve via
                // the grantor's choice; preset to Creature (harmless for the
                // `FromPlayer` arm).
                state
                    .objects
                    .get_mut(&grantor)
                    .unwrap()
                    .chosen_attributes
                    .push(ChosenAttribute::CardType(CoreType::Creature));
                for quality in [first, second] {
                    state
                        .objects
                        .get_mut(&grantor)
                        .unwrap()
                        .static_definitions
                        .push(
                            StaticDefinition::new(StaticMode::PlayerProtection(quality)).affected(
                                TargetFilter::Typed(
                                    TypedFilter::default().controller(ControllerRef::You),
                                ),
                            ),
                        );
                }
            }
        }
    }

    /// Answer the live `ReplacementChoice` prompt via the real `GameAction`
    /// through `apply()`, folding the answer's events into `events`. The
    /// caller sets `state.waiting_for` from the returned prompt first.
    fn answer_replacement_choice(state: &mut GameState, events: &mut Vec<GameEvent>, index: usize) {
        let result =
            crate::game::engine::apply_as_current(state, GameAction::ChooseReplacement { index })
                .expect("the CR 616.1 answer is legal");
        events.extend(result.events);
    }

    /// Host a Comeuppance-class reflector on `host` (SelfRef: damage TO the
    /// host): `Prevention::All` with a per-event reflection rider dealing the
    /// prevented amount back to the damage source (CR 615.5 binds each
    /// reflection to its own source, so the rider keeps its per-event stash).
    fn install_comeuppance_reflector(state: &mut GameState, host: ObjectId) {
        use crate::types::ability::{PreventionAmount, ResolvedAbility};
        let controller = state.objects[&host].controller;
        let rider = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::PostReplacementDamageSource,
                damage_source: None,
                excess: None,
            },
            vec![],
            host,
            controller,
        );
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::All)
                    .valid_card(TargetFilter::SelfRef)
                    .runtime_execute(rider)
                    .description("Comeuppance-class reflector".to_string()),
            );
    }

    /// Host a `Next(n)` shield on `host` (SelfRef: damage TO the host),
    /// optionally with a reflection rider (Comeuppance-class partial).
    fn install_next_shield(
        state: &mut GameState,
        host: ObjectId,
        n: u32,
        name: &str,
        reflecting: bool,
    ) {
        use crate::types::ability::{PreventionAmount, ResolvedAbility};
        let mut def = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .prevention_shield(PreventionAmount::Next(n))
            .valid_card(TargetFilter::SelfRef)
            .description(name.to_string());
        if reflecting {
            let controller = state.objects[&host].controller;
            def = def.runtime_execute(ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                    target: TargetFilter::PostReplacementDamageSource,
                    damage_source: None,
                    excess: None,
                },
                vec![],
                host,
                controller,
            ));
        }
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .replacement_definitions
            .push(def);
    }

    /// Return the candidate index hosted on `source`.
    fn candidate_index_for_source(waiting: &WaitingFor, source: ObjectId) -> usize {
        let WaitingFor::ReplacementChoice { candidates, .. } = waiting else {
            panic!("expected a ReplacementChoice prompt, got {waiting:?}");
        };
        candidates
            .iter()
            .position(|c| c.source_id == source)
            .unwrap_or_else(|| panic!("no candidate hosted on {source:?}: {candidates:?}"))
    }

    fn expect_order_prompt(waiting: &WaitingFor, player: PlayerId, count: usize) {
        use crate::types::game_state::ReplacementChoiceKind;
        let WaitingFor::ReplacementChoice {
            player: chooser,
            candidate_count,
            kind,
            ..
        } = waiting
        else {
            panic!("expected a ReplacementChoice prompt, got {waiting:?}");
        };
        assert_eq!(*chooser, player, "the affected object's controller chooses");
        assert_eq!(*candidate_count, count, "candidate count");
        assert_eq!(*kind, ReplacementChoiceKind::Order, "overlap prompts order");
    }

    /// CR 616.1 (= F1-N1 + N2 core): a two-quality protection overlap pauses
    /// the combat-damage batch EXACTLY once (one prompt, nothing applied),
    /// and the CR 616.1 answer settles the whole batch — exactly one
    /// aggregate prevention event, one simultaneity.
    ///
    /// REVERT PROBES: restore the gate's `is_combat ||` exemption → no prompt
    /// (the first `.expect` fails); restore the batch fn's
    /// clear-and-`None` arm → no prompt either; skip the answer stash → the
    /// completion asserts fail (the paused event is dropped or double-dealt).
    #[test]
    fn f1_t1_protection_overlap_pauses_once_and_resumes_as_one_batch() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "RG Attacker", 3, 3);
        paint(&mut state, attacker, vec![ManaColor::Red, ManaColor::Green]);
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Bear", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        // Pause: ONE prompt, ZERO application.
        let mut events = Vec::new();
        let waiting =
            resolve_combat_damage(&mut state, &mut events).expect("the overlap pauses the batch");
        expect_order_prompt(&waiting, PlayerId(1), 2);
        let record = state
            .pending_combat_damage
            .as_ref()
            .expect("the batch parks its pre-application record");
        assert_eq!(
            (
                record.paused_entry,
                record.decided.len(),
                record.remaining.len()
            ),
            (0, 0, 1),
            "the overlap pauses first (empty decided prefix); the blocker's \
             return damage waits in remaining"
        );
        assert!(
            state.pending_replacement.is_some(),
            "the resume authority stays populated"
        );
        assert_eq!(
            (state.players[0].life, state.players[1].life),
            (20, 20),
            "no life moves before the answer"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::DamageDealt { .. })),
            "no damage is dealt before the answer, got {events:?}"
        );

        // Answer through the real choice authority; the batch completes.
        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the batch settles after its choice, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_damage.is_none());
        assert!(state.pending_replacement.is_none());
        assert!(
            state.combat.as_ref().unwrap().regular_damage_done,
            "the sub-step completes through the resume"
        );

        // ONE simultaneity: exactly one aggregate prevention for E; the
        // blocker's return damage (remaining) still lands.
        let prevented: Vec<(TargetRef, u32)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented { target, amount, .. } => {
                    Some((target.clone(), *amount))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            prevented,
            vec![(TargetRef::Object(blocker), 3)],
            "one aggregate prevention for the whole pause, got {events:?}"
        );
        let dealt: Vec<(TargetRef, u32)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamageDealt { target, amount, .. } => Some((target.clone(), *amount)),
                _ => None,
            })
            .collect();
        assert_eq!(
            dealt,
            vec![(TargetRef::Object(attacker), 2)],
            "only the blocker's return damage is dealt, got {events:?}"
        );
        assert_eq!(
            (state.players[0].life, state.players[1].life),
            (20, 20),
            "creature damage moves no life"
        );
    }

    /// Paired guard for T1: a SINGLE applicable quality still claims directly
    /// in combat (no choice exists) — no prompt, inline completion, same
    /// aggregate shape.
    #[test]
    fn f1_single_quality_claims_without_prompt_in_combat() {
        use crate::types::keywords::{Keyword, ProtectionTarget};
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Red Attacker", 3, 3);
        paint(&mut state, attacker, vec![ManaColor::Red]);
        let blocker = create_creature(&mut state, PlayerId(1), "Guarded Bear", 2, 2);
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .keywords
            .push(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)));
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events);
        assert!(
            !matches!(waiting, Some(WaitingFor::ReplacementChoice { .. })),
            "a single quality claims: no ordering prompt, got {waiting:?}"
        );
        assert!(state.pending_combat_damage.is_none());
        assert!(
            state.combat.as_ref().unwrap().regular_damage_done,
            "the sub-step completes inline"
        );
        let prevented: Vec<(TargetRef, u32)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented { target, amount, .. } => {
                    Some((target.clone(), *amount))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            prevented,
            vec![(TargetRef::Object(blocker), 3)],
            "the claimed prevention publishes the same aggregate shape, got {events:?}"
        );
    }

    /// CR 510.4: a first-strike batch and the regular batch pause
    /// INDEPENDENTLY. A's answer completes A fully before B parks, and cannot
    /// change what B's prompt offers.
    #[test]
    fn f1_t2_first_strike_and_regular_batches_pause_independently() {
        use crate::types::keywords::{Keyword, ProtectionTarget};
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let striker = create_creature(&mut state, PlayerId(0), "RG Striker", 2, 2);
        paint(&mut state, striker, vec![ManaColor::Red, ManaColor::Green]);
        state
            .objects
            .get_mut(&striker)
            .unwrap()
            .keywords
            .push(Keyword::FirstStrike);
        let bear_a = create_creature(&mut state, PlayerId(1), "Warded Bear A", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(bear_a),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        let brute = create_creature(&mut state, PlayerId(0), "RG Brute", 3, 3);
        paint(&mut state, brute, vec![ManaColor::Red, ManaColor::Green]);
        let bear_b = create_creature(&mut state, PlayerId(1), "Warded Bear B", 3, 3);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(bear_b),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        setup_combat(
            &mut state,
            vec![striker, brute],
            vec![(striker, vec![bear_a]), (brute, vec![bear_b])],
        );

        // Batch A (first-strike) pauses.
        let mut events = Vec::new();
        let waiting_a = resolve_combat_damage(&mut state, &mut events).expect("batch A pauses");
        expect_order_prompt(&waiting_a, PlayerId(1), 2);

        // A's answer completes A; the MANDATORY second sub-step runs and B parks.
        state.waiting_for = waiting_a.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        let waiting_b = state.waiting_for.clone();
        expect_order_prompt(&waiting_b, PlayerId(1), 2);
        let pending = state
            .pending_replacement
            .as_ref()
            .expect("B's pause holds its own authority");
        assert!(
            matches!(&pending.proposed, ProposedEvent::Damage { target, .. }
                if *target == TargetRef::Object(bear_b)),
            "B's prompt offers B's event — A's answer cannot change it, got {:?}",
            pending.proposed
        );
        let prevented_a = events
            .iter()
            .filter(|e| {
                matches!(e, GameEvent::DamagePrevented { target, .. }
                    if *target == TargetRef::Object(bear_a))
            })
            .count();
        assert_eq!(
            prevented_a, 1,
            "A completed fully (its aggregate published) before B parked"
        );

        // B's answer settles everything.
        state.waiting_for = waiting_b.clone();
        answer_replacement_choice(&mut state, &mut events, 1);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "both batches settle, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_damage.is_none());
        assert!(state.combat.as_ref().unwrap().regular_damage_done);
        let prevented: Vec<(TargetRef, u32)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented { target, amount, .. } => {
                    Some((target.clone(), *amount))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            prevented,
            vec![
                (TargetRef::Object(bear_a), 2),
                (TargetRef::Object(bear_b), 3),
            ],
            "each batch publishes its own aggregate, got {events:?}"
        );
        assert_eq!(
            (state.players[0].life, state.players[1].life),
            (20, 20),
            "prevention moves no life"
        );
    }

    /// CR 616.1 + CR 510.2 (F1-N2): the resumed batch emits the SAME order as
    /// the inline path — lifelink gain, then the per-player aggregate, then
    /// the prevention rider — because completion runs the SAME shared tail.
    /// Mirrors `completed_batch_emits_gain_then_aggregate_then_prevention_rider`.
    #[test]
    fn f1_t3_resumed_batch_emits_gain_then_aggregate_then_prevention() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let lifelinker = create_creature(&mut state, PlayerId(0), "Lifelink Cat", 2, 2);
        state
            .objects
            .get_mut(&lifelinker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        let attacker = create_creature(&mut state, PlayerId(0), "RG Attacker", 3, 3);
        paint(&mut state, attacker, vec![ManaColor::Red, ManaColor::Green]);
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Bear", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        setup_combat(
            &mut state,
            vec![lifelinker, attacker],
            vec![(attacker, vec![blocker])],
        );

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("the overlap pauses");
        expect_order_prompt(&waiting, PlayerId(1), 2);
        let record = state.pending_combat_damage.as_ref().unwrap();
        assert_eq!(
            (
                record.paused_entry,
                record.decided.len(),
                record.remaining.len()
            ),
            (1, 1, 1),
            "the lifelinker is decided; the overlap pauses second; the \
             blocker's return damage waits in remaining"
        );

        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 1);

        let index_of = |pred: &dyn Fn(&GameEvent) -> bool| {
            events
                .iter()
                .position(pred)
                .unwrap_or_else(|| panic!("event missing from {events:?}"))
        };
        let gain =
            index_of(&|e| matches!(e, GameEvent::LifeChanged { amount, .. } if *amount == 2));
        let aggregate = index_of(&|e| matches!(e, GameEvent::CombatDamageDealtToPlayer { .. }));
        let prevention =
            index_of(&|e| matches!(e, GameEvent::DamagePrevented { amount, .. } if *amount == 3));
        assert!(
            gain < aggregate && aggregate < prevention,
            "resumed batch order must be gain ({gain}) < aggregate ({aggregate}) \
             < prevention ({prevention}), got {events:?}"
        );
        assert_eq!(state.players[0].life, 22, "the lifelinker gained 2");
        assert_eq!(state.players[1].life, 18, "the lifelinker dealt 2");
        assert_eq!(
            state.objects[&attacker].damage_marked, 2,
            "the blocker's return damage (remaining) still lands"
        );
        assert!(state.pending_combat_damage.is_none());
    }

    /// CR 616.1f: TWO-LEVEL pause. E (protection overlap + a Comeuppance-class
    /// reflector) is answered by applying the reflector: the Prevented head
    /// stashes `None`, the head inline drain fires the reflection rider, and
    /// the reflection's own overlap parks a NESTED choice N. N's answer
    /// delivers inline exactly once (never stashed into E's slot, never
    /// lost); every leg folds into ONE batch.
    ///
    /// REVERT PROBES: drop the Prevented-head drain → no N prompt (reflection
    /// never fires); let nested answers stash (delete the len gate) → the
    /// alignment asserts fail (E's slot overflows); drop the fold+claim →
    /// the nested legs strand outside the batch (counts/order fail).
    #[test]
    fn f1_t4_nested_pause_during_answer_delivers_inline_and_folds_one_batch() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "RG Attacker", 5, 5);
        paint(&mut state, attacker, vec![ManaColor::Red, ManaColor::Green]);
        install_next_shield(&mut state, attacker, 2, "Ward of Embers", false);
        install_next_shield(&mut state, attacker, 1, "Ward of Thorns", false);
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Bear", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        install_comeuppance_reflector(&mut state, blocker);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        // E pauses: two virtual protections + the reflector.
        let mut events = Vec::new();
        let waiting_e = resolve_combat_damage(&mut state, &mut events).expect("E pauses the batch");
        expect_order_prompt(&waiting_e, PlayerId(1), 3);

        // Answer E by applying the reflector: stash-then-surface parks N.
        state.waiting_for = waiting_e.clone();
        let reflector = candidate_index_for_source(&waiting_e, blocker);
        answer_replacement_choice(&mut state, &mut events, reflector);
        let waiting_n = state.waiting_for.clone();
        expect_order_prompt(&waiting_n, PlayerId(0), 2);
        let pending = state
            .pending_replacement
            .as_ref()
            .expect("N's pause holds its own authority");
        assert!(
            matches!(&pending.proposed, ProposedEvent::Damage { target, .. }
                if *target == TargetRef::Object(attacker)),
            "N owns the REFLECTION damage to the attacker, got {:?}",
            pending.proposed
        );
        let record = state.pending_combat_damage.as_ref().unwrap();
        assert_eq!(
            (record.paused_entry, record.decided.len()),
            (0, 1),
            "E's slot is stashed (None: prevented) while N is open"
        );
        assert_eq!(record.decided[0], None);
        assert!(!record.paused_entry_pruned);

        // Answer N: the nested damage delivers inline exactly once, and the
        // batch completes through the resume.
        state.waiting_for = waiting_n.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the batch settles after N, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_damage.is_none());
        assert!(state.combat.as_ref().unwrap().regular_damage_done);

        // (b) nested delivery: 5 reflected − 2 − 1 = 2, exactly once —
        // plus the blocker's 2 return damage (remaining), for 4 marked.
        assert_eq!(
            state.objects[&attacker].damage_marked, 4,
            "reflection 2 + blocker return 2"
        );
        assert_eq!(
            state.objects[&blocker].damage_marked, 0,
            "E was prevented: the blocker takes nothing"
        );
        let dealt_to_attacker: Vec<(u32, bool)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamageDealt {
                    target,
                    amount,
                    is_combat,
                    ..
                } if *target == TargetRef::Object(attacker) => Some((*amount, *is_combat)),
                _ => None,
            })
            .collect();
        assert_eq!(
            dealt_to_attacker,
            vec![(2, false), (2, true)],
            "the nested (noncombat) damage is delivered exactly once, ahead of \
             the blocker's (combat) return damage, got {events:?}"
        );
        // (a) no overflow past the paused slot: E's prevention is shield-keyed
        // (source = the reflector's host), never protection-keyed.
        let prevented: Vec<(ObjectId, TargetRef, u32)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented {
                    source_id,
                    target,
                    amount,
                } => Some((*source_id, target.clone(), *amount)),
                _ => None,
            })
            .collect();
        assert!(
            prevented.contains(&(blocker, TargetRef::Object(blocker), 5)),
            "E's prevention publishes under the chosen SHIELD, got {prevented:?}"
        );
        // (c) ONE batch: N's legs fold in ahead of E's aggregate.
        assert!(
            prevented.contains(&(attacker, TargetRef::Object(attacker), 2))
                && prevented.contains(&(attacker, TargetRef::Object(attacker), 1)),
            "N's partial preventions join the same batch, got {prevented:?}"
        );
        let nested_dealt = events
            .iter()
            .position(|e| {
                matches!(e, GameEvent::DamageDealt { target, .. }
                    if *target == TargetRef::Object(attacker))
            })
            .unwrap();
        let e_aggregate = events
            .iter()
            .position(|e| {
                matches!(e, GameEvent::DamagePrevented { target, amount, .. }
                    if *target == TargetRef::Object(blocker) && *amount == 5)
            })
            .unwrap();
        assert!(
            nested_dealt < e_aggregate,
            "the nested leg folds AHEAD of E's aggregate in the one batch"
        );
    }

    /// Host an Awe-Strike-class shield on `host` (SelfRef: damage TO the
    /// host): `Next(n)` with a gain-life rider paying its controller the
    /// prevented amount. Unlike a reflection rider (whose
    /// `PostReplacementDamageSource` only the PREVENTED arm populates), the
    /// fixed-target gain resolves on the Execute path too.
    fn install_gain_rider_shield(state: &mut GameState, host: ObjectId, n: u32, name: &str) {
        use crate::types::ability::{PreventionAmount, ResolvedAbility};
        let controller = state.objects[&host].controller;
        let rider = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                player: TargetFilter::Controller,
            },
            vec![],
            host,
            controller,
        );
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::Next(n))
                    .valid_card(TargetFilter::SelfRef)
                    .runtime_execute(rider)
                    .description(name.to_string()),
            );
    }

    /// B5 companion to T4: E resolves through the DAMAGE arm (Execute + a
    /// stashed gain rider), so the Execute EPILOGUE drains the rider and its
    /// life-gain replacement overlap parks N. A Damage-arm inline drain would
    /// run BEFORE the epilogue's unconditional `waiting_for = Priority`
    /// reset and clobber N's prompt — the N-surfaces assertion below is that
    /// probe.
    #[test]
    fn f1_t4b_execute_answer_drains_rider_in_epilogue_and_parks_nested() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Ogre", 8, 8);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 5, 5);
        install_gain_rider_shield(&mut state, blocker, 3, "Awe-Strike Ward");
        install_next_shield(&mut state, blocker, 3, "Plain Ward", false);
        install_gain_life_replacement(
            &mut state,
            PlayerId(1),
            "Rhox Faithmender",
            doubler_amount(),
        );
        install_gain_life_replacement(
            &mut state,
            PlayerId(1),
            "Leyline of Hope",
            plus_one_amount(),
        );
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        // E pauses: two partial shields, order-material.
        let mut events = Vec::new();
        let waiting_e = resolve_combat_damage(&mut state, &mut events).expect("E pauses the batch");
        expect_order_prompt(&waiting_e, PlayerId(1), 2);

        // Answer E: 8 − 3 − 3 = 2 survives (Execute); the epilogue drain
        // fires the gain rider (3) and its replacement overlap parks N.
        state.waiting_for = waiting_e.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        let waiting_n = state.waiting_for.clone();
        expect_order_prompt(&waiting_n, PlayerId(1), 2);
        let pending = state
            .pending_replacement
            .as_ref()
            .expect("N's pause holds its own authority");
        assert!(
            matches!(&pending.proposed, ProposedEvent::LifeGain { .. }),
            "N owns the rider's LIFE GAIN, got {:?}",
            pending.proposed
        );
        let record = state.pending_combat_damage.as_ref().unwrap();
        assert_eq!(
            (record.paused_entry, record.decided.len()),
            (0, 1),
            "E's Execute survivor is stashed while N is open"
        );
        assert!(
            matches!(&record.decided[0], Some(ProposedEvent::Damage { amount, .. })
                if *amount == 2),
            "E's stash carries the 2 surviving damage, got {:?}",
            record.decided[0]
        );

        // Answer N: the gain applies inline (modified exactly once); the
        // batch completes through the resume.
        state.waiting_for = waiting_n.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the batch settles after N, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_damage.is_none());
        assert!(state.pending_combat_lifelink.is_none());
        let gains: Vec<i32> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::LifeChanged {
                    player_id, amount, ..
                } if *player_id == PlayerId(1) => Some(*amount),
                _ => None,
            })
            .collect();
        assert_eq!(
            gains.len(),
            1,
            "the nested gain applies exactly once, got {events:?}"
        );
        assert!(
            [27, 28].contains(&(20 + gains[0])),
            "doubler + plus-one order the 3 exactly once (2*3+1 or 2*(3+1)), got {gains:?}"
        );
        assert_eq!(state.players[1].life, 20 + gains[0]);
        assert_eq!(
            state.objects[&blocker].damage_marked, 2,
            "E's stashed survivor applies exactly once"
        );
        assert_eq!(
            state.objects[&attacker].damage_marked, 5,
            "the blocker's return damage (remaining) still lands"
        );
    }

    /// CR 702.15a + CR 510.2: lifelink + shields on the SAME paused damage —
    /// the gain is simultaneous with the damage (same trigger batch), and the
    /// pause surfaces a `ReplacementChoice`, never a priority window.
    #[test]
    fn f1_n3_lifelink_gain_is_simultaneous_with_paused_damage() {
        let mut state = setup();
        let lifelinker = create_creature(&mut state, PlayerId(0), "Lifelink Ogre", 5, 5);
        state
            .objects
            .get_mut(&lifelinker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 5, 5);
        install_next_shield(&mut state, blocker, 2, "Ward A", false);
        install_next_shield(&mut state, blocker, 2, "Ward B", false);
        setup_combat(
            &mut state,
            vec![lifelinker],
            vec![(lifelinker, vec![blocker])],
        );

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("the overlap pauses");
        assert!(
            matches!(waiting, WaitingFor::ReplacementChoice { .. }),
            "a CR 616.1 pause is a choice, never a priority window, got {waiting:?}"
        );
        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 0);

        // 5 − 2 − 2 = 1 dealt; the gain is simultaneous, exactly once.
        assert_eq!(state.objects[&blocker].damage_marked, 1);
        assert_eq!(
            state.players[0].life, 21,
            "the lifelinker gains its 1 dealt"
        );
        let gains: Vec<i32> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::LifeChanged {
                    player_id, amount, ..
                } if *player_id == PlayerId(0) => Some(*amount),
                _ => None,
            })
            .collect();
        assert_eq!(
            gains,
            vec![1],
            "exactly one simultaneous gain, got {events:?}"
        );
        let dealt = events
            .iter()
            .position(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .unwrap();
        let gain = events
            .iter()
            .position(|e| matches!(e, GameEvent::LifeChanged { .. }))
            .unwrap();
        assert!(
            dealt < gain,
            "the paused batch keeps the inline relative order (dealt before gain)"
        );

        // Paired guard: the same combat unpaused gains the full 5 in the same
        // relative order.
        let mut control = setup();
        let lifelinker = create_creature(&mut control, PlayerId(0), "Lifelink Ogre", 5, 5);
        control
            .objects
            .get_mut(&lifelinker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        let blocker = create_creature(&mut control, PlayerId(1), "Bear", 5, 5);
        setup_combat(
            &mut control,
            vec![lifelinker],
            vec![(lifelinker, vec![blocker])],
        );
        let mut control_events = Vec::new();
        let control_waiting = resolve_combat_damage(&mut control, &mut control_events);
        assert!(
            !matches!(control_waiting, Some(WaitingFor::ReplacementChoice { .. })),
            "the control never pauses"
        );
        assert_eq!(control.players[0].life, 25);
        let dealt = control_events
            .iter()
            .position(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .unwrap();
        let gain = control_events
            .iter()
            .position(|e| matches!(e, GameEvent::LifeChanged { .. }))
            .unwrap();
        assert!(dealt < gain, "the control keeps the same relative order");
    }

    /// CR 704.6c: a batch paused on a protection choice still accrues
    /// commander damage EXACTLY the dealt amount, EXACTLY once — the resume
    /// never re-runs Phase C bookkeeping.
    ///
    /// REVERT PROBE: run the bookkeeping on both the park and the resume (or
    /// apply survivors twice) → the accrual reads 6 or duplicated entries.
    #[test]
    fn f1_n4_commander_damage_accrues_exactly_once_through_pause() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let commander = create_creature(&mut state, PlayerId(0), "Commander", 3, 3);
        state.objects.get_mut(&commander).unwrap().is_commander = true;
        let attacker = create_creature(&mut state, PlayerId(0), "RG Attacker", 3, 3);
        paint(&mut state, attacker, vec![ManaColor::Red, ManaColor::Green]);
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Bear", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        setup_combat(
            &mut state,
            vec![commander, attacker],
            vec![(attacker, vec![blocker])],
        );

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("the overlap pauses");
        expect_order_prompt(&waiting, PlayerId(1), 2);
        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 1);

        assert_eq!(state.players[1].life, 17, "the commander dealt 3");
        assert_eq!(
            state.commander_damage.len(),
            1,
            "exactly one accrual entry, got {:?}",
            state.commander_damage
        );
        let entry = &state.commander_damage[0];
        assert_eq!(
            (entry.player, entry.commander, entry.damage),
            (PlayerId(1), commander, 3),
            "exactly the dealt amount, exactly once"
        );
        assert!(state.pending_combat_damage.is_none());
    }

    /// CR 615.7 + CR 615.13: the order choice consumes the CHOSEN shield
    /// exactly as on the inline path; the unchosen shield keeps its charges;
    /// the tally records only this batch's prevented per (shield, recipient).
    #[test]
    fn f1_n6_order_choice_consumes_chosen_shield_only() {
        use crate::types::ability::ShieldKind;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Ogre", 5, 5);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 5, 5);
        install_next_shield(&mut state, blocker, 5, "Ward A", false);
        install_next_shield(&mut state, blocker, 5, "Ward B", false);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("two shields pause");
        expect_order_prompt(&waiting, PlayerId(1), 2);
        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 0);

        // The first shield absorbed all 5 (consumed); the second never fired.
        let host = state.objects.get(&blocker).unwrap();
        let [first, second] = host.replacement_definitions.as_slice() else {
            panic!(
                "two shields expected, got {:?}",
                host.replacement_definitions
            );
        };
        assert!(
            first.is_consumed,
            "the chosen shield is consumed, got {first:?}"
        );
        assert!(
            matches!(
                second.shield_kind,
                ShieldKind::Prevention {
                    amount: crate::types::ability::PreventionAmount::Next(5),
                }
            ) && !second.is_consumed,
            "the unchosen shield keeps its charges, got {second:?}"
        );
        let prevented: Vec<u32> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented { amount, .. } => Some(*amount),
                _ => None,
            })
            .collect();
        assert_eq!(
            prevented,
            vec![5],
            "the tally records only this batch's prevented, got {events:?}"
        );
        assert_eq!(state.objects[&blocker].damage_marked, 0);
        assert!(state.pending_combat_damage.is_none());

        // Paired guard: a lone Next(5) inline ends in the same consumed state.
        let mut control = setup();
        let attacker = create_creature(&mut control, PlayerId(0), "Ogre", 5, 5);
        let blocker = create_creature(&mut control, PlayerId(1), "Bear", 5, 5);
        install_next_shield(&mut control, blocker, 5, "Ward A", false);
        setup_combat(
            &mut control,
            vec![attacker],
            vec![(attacker, vec![blocker])],
        );
        let mut control_events = Vec::new();
        let control_waiting = resolve_combat_damage(&mut control, &mut control_events);
        assert!(
            !matches!(control_waiting, Some(WaitingFor::ReplacementChoice { .. })),
            "a lone shield never pauses"
        );
        let lone = &control
            .objects
            .get(&blocker)
            .unwrap()
            .replacement_definitions[0];
        assert_eq!(
            (first.is_consumed, lone.is_consumed),
            (true, true),
            "chosen-via-pause consumes exactly as chosen-inline"
        );
    }

    /// CR 732.2a: the record carries the PRE-BATCH life totals (never
    /// re-snapshotted), and the pause-path observation fires at every park —
    /// an inline rider leg's life move invalidates the ring AT THE PAUSE.
    /// (First real ring test for a parked batch: gains/losses apply through
    /// `apply_*` with no ring observation of their own, so only the
    /// pause-path call can clear the seeded ring here.)
    #[test]
    fn f1_n7_pause_carries_lives_before_and_observes_inline_life_move() {
        use crate::types::ability::{PreventionAmount, ResolvedAbility};
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        // Event 1: absorbed by an Awe-Strike-class Next shield whose rider
        // gains its controller the prevented amount (inline life move).
        let ogre = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
        let squire = create_creature(&mut state, PlayerId(1), "Squire", 3, 3);
        let rider = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                player: TargetFilter::Controller,
            },
            vec![],
            squire,
            PlayerId(1),
        );
        state
            .objects
            .get_mut(&squire)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::Next(3))
                    .valid_card(TargetFilter::SelfRef)
                    .runtime_execute(rider)
                    .description("Awe-Strike-class shield".to_string()),
            );
        // Event 2: the protection overlap that pauses.
        let attacker = create_creature(&mut state, PlayerId(0), "RG Attacker", 3, 3);
        paint(&mut state, attacker, vec![ManaColor::Red, ManaColor::Green]);
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Bear", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        setup_combat(
            &mut state,
            vec![ogre, attacker],
            vec![(ogre, vec![squire]), (attacker, vec![blocker])],
        );

        // Seed the ring (mirrors the loop-test seeding pattern).
        for life in [40, 39, 38] {
            state.players[1].life = life;
            state.record_loop_detect_sample();
        }
        assert_eq!(
            state.loop_detect_ring.len(),
            3,
            "reach guard: the ring is seeded before the batch"
        );

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("event 2 pauses");
        expect_order_prompt(&waiting, PlayerId(1), 2);
        assert!(
            state.loop_detect_ring.is_empty(),
            "the pause-path observation fired: the inline 38 -> 41 gain \
             invalidated the ring AT THE PARK"
        );
        let record = state.pending_combat_damage.as_ref().unwrap();
        assert_eq!(
            record.lives_before,
            vec![20, 38],
            "lives_before is the PRE-BATCH snapshot, carried — never re-snapshotted"
        );
        assert_eq!(
            (record.paused_entry, record.decided.len()),
            (1, 1),
            "event 1 decided (its prevention tallied); event 2 paused"
        );
        assert!(
            record
                .phase_b_tally
                .iter()
                .any(|entry| { entry.recipient == TargetRef::Object(squire) && entry.amount == 3 }),
            "the inline leg's prevention is already tallied at the pause, got {:?}",
            record.phase_b_tally
        );

        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert_eq!(state.players[1].life, 41, "the inline gain stands");
        assert!(state.pending_combat_damage.is_none());
        assert!(state.loop_detect_ring.is_empty());
    }

    /// CR 615.13 + CR 120.3: a fully prevented PAUSED event still publishes
    /// its aggregate (recipient + amount — exactly what a Squire-class
    /// "damage that would be dealt to you" trigger reads) into the ONE batch.
    #[test]
    fn f1_n8_prevented_paused_event_publishes_its_aggregate() {
        use crate::types::ability::ControllerRef;
        use crate::types::keywords::ProtectionTarget;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Goblin", 3, 3);
        install_protection_overlap(
            &mut state,
            TargetRef::Player(PlayerId(1)),
            ProtectionTarget::FromPlayer(ControllerRef::Opponent),
            ProtectionTarget::ChosenCardType,
        );
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("the overlap pauses");
        expect_order_prompt(&waiting, PlayerId(1), 2);
        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 0);

        let prevented: Vec<(ObjectId, TargetRef, u32)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented {
                    source_id,
                    target,
                    amount,
                } => Some((*source_id, target.clone(), *amount)),
                _ => None,
            })
            .collect();
        assert_eq!(
            prevented,
            vec![(ObjectId(0), TargetRef::Player(PlayerId(1)), 3)],
            "protection-keyed aggregate (recipient + amount) publishes exactly once, got {events:?}"
        );
        assert_eq!(state.players[1].life, 20);
        assert!(state.pending_combat_damage.is_none());

        // Paired guard: the Inkshield inline path emits the same shape —
        // one aggregate, same recipient and amount (its own shield source).
        let mut control = setup();
        install_inkshield(&mut control, PlayerId(1));
        let attacker = create_creature(&mut control, PlayerId(0), "Ogre", 3, 3);
        setup_combat(&mut control, vec![attacker], vec![]);
        let mut control_events = Vec::new();
        let control_waiting = resolve_combat_damage(&mut control, &mut control_events);
        assert!(
            !matches!(control_waiting, Some(WaitingFor::ReplacementChoice { .. })),
            "a lone shield never pauses"
        );
        let prevented: Vec<(TargetRef, u32)> = control_events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented { target, amount, .. } => {
                    Some((target.clone(), *amount))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            prevented,
            vec![(TargetRef::Player(PlayerId(1)), 3)],
            "same aggregate shape on the inline path, got {control_events:?}"
        );
    }

    /// Class instance 2 (matrix F1-N1): protection-from-creatures stacked with
    /// protection-from-red (distinct grants) vs a red creature's combat
    /// damage — pauses, orders, completes.
    #[test]
    fn f1_instance_stacked_type_and_color_protections_pause() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Red Goblin", 3, 3);
        paint(&mut state, attacker, vec![ManaColor::Red]);
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Bear", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker),
            ProtectionTarget::CardType("creatures".to_string()),
            ProtectionTarget::Color(ManaColor::Red),
        );
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("the overlap pauses");
        expect_order_prompt(&waiting, PlayerId(1), 2);
        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 1);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the batch settles, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_damage.is_none());
        let prevented: Vec<u32> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented { amount, .. } => Some(*amount),
                _ => None,
            })
            .collect();
        assert_eq!(prevented, vec![3]);
        assert_eq!((state.players[0].life, state.players[1].life), (20, 20));
    }

    /// Class instance 3: player-scope overlap (protection-from-opponents +
    /// protection-from-creatures on the defending player, one Emissary-class
    /// static each) pauses with the affected player choosing, and completes
    /// as one batch.
    #[test]
    fn f1_instance_player_scope_overlap_pauses() {
        use crate::types::ability::ControllerRef;
        use crate::types::keywords::ProtectionTarget;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Goblin", 3, 3);
        install_protection_overlap(
            &mut state,
            TargetRef::Player(PlayerId(1)),
            ProtectionTarget::FromPlayer(ControllerRef::Opponent),
            ProtectionTarget::ChosenCardType,
        );
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("the overlap pauses");
        expect_order_prompt(&waiting, PlayerId(1), 2);
        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the batch settles, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_damage.is_none());
        assert!(state.combat.as_ref().unwrap().regular_damage_done);
        assert_eq!(state.players[1].life, 20, "the damage was prevented");
    }

    /// Class instance 4 (cross-family): a protection overlap plus a registry
    /// `Prevention::All` shield co-applicable on the same combat damage —
    /// the prompt orders virtual candidates vs the shield, and the chosen
    /// shield's prevention (not the protections') publishes.
    ///
    /// Two qualities, not one: a single quality claims in the gate and draws
    /// no virtual candidate, so cross-family ordering NEEDS the overlap.
    #[test]
    fn f1_instance_cross_family_orders_virtual_vs_shield() {
        use crate::types::ability::PreventionAmount;
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "RG Attacker", 3, 3);
        paint(&mut state, attacker, vec![ManaColor::Red, ManaColor::Green]);
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Bear", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::All)
                    .valid_card(TargetFilter::SelfRef)
                    .description("Bulwark shield".to_string()),
            );
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("the overlap pauses");
        expect_order_prompt(&waiting, PlayerId(1), 3);
        state.waiting_for = waiting.clone();
        let shield = candidate_index_for_source(&waiting, blocker);
        answer_replacement_choice(&mut state, &mut events, shield);

        let prevented: Vec<(ObjectId, TargetRef, u32)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented {
                    source_id,
                    target,
                    amount,
                } => Some((*source_id, target.clone(), *amount)),
                _ => None,
            })
            .collect();
        assert_eq!(
            prevented,
            vec![(blocker, TargetRef::Object(blocker), 3)],
            "the CHOSEN shield's prevention publishes (provenance: the shield's \
             host, not the protection sentinel), got {events:?}"
        );
        assert!(state.pending_combat_damage.is_none());
    }

    /// CR 616.1 (R2): the pausable batch also covers non-protection
    /// NeedsChoice in combat. Two `Prevention::All` shields on one combat
    /// damage pause for shield-vs-shield ordering; the chosen shield absorbs
    /// the batch, its rider fires once vs the aggregate, the other fires
    /// nothing.
    #[test]
    fn f1_r2_shield_vs_shield_orders_and_fires_once() {
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));
        install_inkshield(&mut state, PlayerId(1));
        let attacker = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("two shields pause");
        expect_order_prompt(&waiting, PlayerId(1), 2);
        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 0);

        let prevented: Vec<(TargetRef, u32)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented { target, amount, .. } => {
                    Some((target.clone(), *amount))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            prevented,
            vec![(TargetRef::Player(PlayerId(1)), 3)],
            "one aggregate for the whole pause, got {events:?}"
        );
        assert_eq!(
            count_inklings(&state),
            3,
            "the chosen shield's rider fires once vs the aggregate; the other fires nothing"
        );
        assert_eq!(state.players[1].life, 20);
        assert!(state.pending_combat_damage.is_none());
    }

    /// A single OPTIONAL shield in combat pauses with an accept/decline
    /// branch (not an ordering): index 0 applies the shield, index 1
    /// declines into full damage. Both legs complete through the resume.
    #[test]
    fn f1_optional_shield_accepts_and_declines_in_combat() {
        use crate::types::ability::{PreventionAmount, ReplacementMode};
        use crate::types::game_state::ReplacementChoiceKind;

        fn fixture() -> (GameState, ObjectId) {
            let mut state = setup();
            let attacker = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
            let blocker = create_creature(&mut state, PlayerId(1), "Bear", 5, 5);
            state
                .objects
                .get_mut(&blocker)
                .unwrap()
                .replacement_definitions
                .push(
                    ReplacementDefinition::new(ReplacementEvent::DamageDone)
                        .prevention_shield(PreventionAmount::All)
                        .valid_card(TargetFilter::SelfRef)
                        .mode(ReplacementMode::Optional { decline: None })
                        .description("Aegis shield".to_string()),
                );
            setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);
            (state, blocker)
        }
        fn prompt_kind(waiting: &WaitingFor) -> ReplacementChoiceKind {
            let WaitingFor::ReplacementChoice { kind, .. } = waiting else {
                panic!("expected a ReplacementChoice prompt, got {waiting:?}");
            };
            *kind
        }

        // Accept: the shield prevents everything.
        let (mut state, blocker) = fixture();
        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("the option pauses");
        assert_eq!(prompt_kind(&waiting), ReplacementChoiceKind::OptionalBranch);
        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "accept settles, got {:?}",
            state.waiting_for
        );
        assert_eq!(state.objects[&blocker].damage_marked, 0);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 3, .. })),
            "accept prevents all 3, got {events:?}"
        );
        assert!(state.pending_combat_damage.is_none());

        // Decline: full damage lands.
        let (mut state, blocker) = fixture();
        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("the option pauses");
        assert_eq!(prompt_kind(&waiting), ReplacementChoiceKind::OptionalBranch);
        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 1);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "decline settles, got {:?}",
            state.waiting_for
        );
        assert_eq!(
            state.objects[&blocker].damage_marked, 3,
            "decline deals full damage"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
            "decline prevents nothing, got {events:?}"
        );
        assert!(state.pending_combat_damage.is_none());
    }

    /// B6 (unstashed): the paused choice's seat departs mid-pause. Elimination
    /// retains the paused entry and SETS `paused_entry_pruned`; the guard
    /// settle converts the paused slot to `None`, TAKING the flag — and the
    /// batch completes instead of hanging.
    #[test]
    fn f1_guard_settle_unstashed_chooser_loss_completes_batch() {
        use crate::types::format::FormatConfig;
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        let attacker = create_creature(&mut state, PlayerId(0), "RG Attacker", 3, 3);
        paint(&mut state, attacker, vec![ManaColor::Red, ManaColor::Green]);
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Bear", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("the overlap pauses");
        expect_order_prompt(&waiting, PlayerId(1), 2);

        // The chooser departs: pending clears, the paused entry is retained
        // with the prune flag (its owner left with the seat).
        crate::game::elimination::eliminate_player(&mut state, PlayerId(1), &mut events);
        assert!(
            state.pending_replacement.is_none(),
            "the died seat's authority is cleared"
        );
        let record = state.pending_combat_damage.as_ref().unwrap();
        assert_eq!(
            (record.paused_entry, record.decided.len()),
            (0, 0),
            "still unstashed after the prune"
        );
        assert!(
            record.paused_entry_pruned,
            "owner-keyed departure flags the retained paused entry"
        );

        // The guard settles (stash None, take flag) and the batch completes.
        settle_orphaned_combat_damage_choice(&mut state);
        let record = state.pending_combat_damage.as_ref().unwrap();
        assert_eq!(
            record.decided,
            vec![None],
            "the paused choice died: stash None"
        );
        assert!(
            !record.paused_entry_pruned,
            "the settle TAKES the flag on use"
        );
        let settled = resolve_combat_damage(&mut state, &mut events);
        assert!(
            matches!(settled, Some(WaitingFor::Priority { .. }) | None),
            "the batch completes instead of hanging, got {settled:?}"
        );
        assert!(state.pending_combat_damage.is_none());
        assert!(state.combat.as_ref().unwrap().regular_damage_done);
    }

    /// B6 site 3 (N5-cross): a STOLEN creature as paused recipient — the OWNER
    /// concedes while the CONTROLLER (the chooser) lives. The prune flags the
    /// retained paused entry (owner-keyed: the controller's survival does not
    /// matter); the living controller's answer then converts the paused slot
    /// to `None`, TAKING the flag on the answer path.
    #[test]
    fn f1_answer_takes_prune_flag_for_departed_recipient() {
        use crate::types::format::FormatConfig;
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        let attacker = create_creature(&mut state, PlayerId(0), "RG Attacker", 3, 3);
        paint(&mut state, attacker, vec![ManaColor::Red, ManaColor::Green]);
        // Stolen: owned by P1, controlled (and chosen) by the living P2.
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Bear", 2, 2);
        state.objects.get_mut(&blocker).unwrap().controller = PlayerId(2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("the overlap pauses");
        expect_order_prompt(&waiting, PlayerId(2), 2);

        // The owner concedes: the authority SURVIVES (the chooser lives) but
        // the paused entry flags — damage to departed recipients does nothing.
        crate::game::elimination::eliminate_player(&mut state, PlayerId(1), &mut events);
        assert!(
            state.pending_replacement.is_some(),
            "the living chooser's authority survives the owner's departure"
        );
        assert!(
            state
                .pending_combat_damage
                .as_ref()
                .unwrap()
                .paused_entry_pruned,
            "owner-keyed departure flags the retained paused entry"
        );

        // The living controller answers; the stash takes the flag (None) and
        // the batch completes with the departed recipient taking nothing.
        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the batch settles, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_damage.is_none());
        assert!(
            !events.iter().any(|e| matches!(
                e,
                GameEvent::DamageDealt { target, .. } if *target == TargetRef::Object(blocker)
            )),
            "the departed recipient is dealt nothing, got {events:?}"
        );
    }

    /// B6 (stashed): the NESTED choice's seat departs mid-pause. The guard
    /// settle drops the nested loss with NO stash — E's stash is intact
    /// (`len == paused_entry + 1`) — and the batch completes.
    #[test]
    fn f1_guard_settle_stashed_nested_loss_keeps_paused_stash() {
        use crate::types::format::FormatConfig;
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        // T4's shape: E answered (reflector) with N open is the stashed window.
        let attacker = create_creature(&mut state, PlayerId(0), "RG Attacker", 5, 5);
        paint(&mut state, attacker, vec![ManaColor::Red, ManaColor::Green]);
        install_next_shield(&mut state, attacker, 2, "Ward of Embers", false);
        install_next_shield(&mut state, attacker, 1, "Ward of Thorns", false);
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Bear", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        install_comeuppance_reflector(&mut state, blocker);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        let waiting_e = resolve_combat_damage(&mut state, &mut events).expect("E pauses the batch");
        state.waiting_for = waiting_e.clone();
        let reflector = candidate_index_for_source(&waiting_e, blocker);
        answer_replacement_choice(&mut state, &mut events, reflector);
        let waiting_n = state.waiting_for.clone();
        expect_order_prompt(&waiting_n, PlayerId(0), 2);

        // N's chooser (P0) departs: pending clears, E's stash is untouched.
        crate::game::elimination::eliminate_player(&mut state, PlayerId(0), &mut events);
        assert!(
            state.pending_replacement.is_none(),
            "the died seat's authority is cleared"
        );
        let record = state.pending_combat_damage.as_ref().unwrap();
        assert_eq!(
            (record.paused_entry, record.decided.len()),
            (0, 1),
            "E's stash survives N's chooser departing"
        );
        assert_eq!(record.decided[0], None);

        // The guard settle stashes NOTHING (the died choice was nested) and
        // the batch completes.
        settle_orphaned_combat_damage_choice(&mut state);
        let record = state.pending_combat_damage.as_ref().unwrap();
        assert_eq!(
            record.decided.len(),
            1,
            "no nested stash: decided stays exactly the paused slot"
        );
        let settled = resolve_combat_damage(&mut state, &mut events);
        assert!(
            matches!(settled, Some(WaitingFor::Priority { .. }) | None),
            "the batch completes instead of hanging, got {settled:?}"
        );
        assert!(state.pending_combat_damage.is_none());
        assert!(state.combat.as_ref().unwrap().regular_damage_done);
        assert_eq!(
            state.objects[&blocker].damage_marked, 0,
            "E was prevented: the blocker takes nothing"
        );
    }

    /// CR 616.1f: the SAME batch pauses twice. Two warded blockers (E1, E2):
    /// E1's answer resumes into E2's overlap, which re-pauses with a FRESH
    /// record (never a mutated one, so no stale `paused_entry_pruned` survives
    /// across parks). E2's answer completes the one batch.
    ///
    /// REVERT PROBES: route the resume's `Paused` outcome to completion instead
    /// of re-parking → the second prompt never surfaces; mutate the old record
    /// instead of rebuilding → the fresh-park flag/gate asserts fail; fold an
    /// answer span twice → the final multisets duplicate.
    #[test]
    fn f1_resume_repauses_same_batch_on_second_overlap() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let attacker_a = create_creature(&mut state, PlayerId(0), "RG Attacker A", 3, 3);
        paint(
            &mut state,
            attacker_a,
            vec![ManaColor::Red, ManaColor::Green],
        );
        let blocker_a = create_creature(&mut state, PlayerId(1), "Warded Bear A", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker_a),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        let attacker_b = create_creature(&mut state, PlayerId(0), "RG Attacker B", 3, 3);
        paint(
            &mut state,
            attacker_b,
            vec![ManaColor::Red, ManaColor::Green],
        );
        let blocker_b = create_creature(&mut state, PlayerId(1), "Warded Bear B", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker_b),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        setup_combat(
            &mut state,
            vec![attacker_a, attacker_b],
            vec![(attacker_a, vec![blocker_a]), (attacker_b, vec![blocker_b])],
        );

        // E1 parks first: both attackers' hits precede the blockers' returns.
        let mut events = Vec::new();
        let waiting_e1 =
            resolve_combat_damage(&mut state, &mut events).expect("E1 pauses the batch");
        expect_order_prompt(&waiting_e1, PlayerId(1), 2);
        let pending = state
            .pending_replacement
            .as_ref()
            .expect("E1's pause holds its own authority");
        assert!(
            matches!(&pending.proposed, ProposedEvent::Damage { target, .. }
                if *target == TargetRef::Object(blocker_a)),
            "E1 owns the FIRST overlap (attacker A's hit), got {:?}",
            pending.proposed
        );
        let record = state.pending_combat_damage.as_ref().unwrap();
        assert_eq!(
            (
                record.paused_entry,
                record.decided.len(),
                record.remaining.len()
            ),
            (0, 0, 3),
            "E1 pauses first; E2 plus both returns wait in remaining"
        );

        // Answer E1: the resume hits E2's overlap and re-pauses with a FRESH
        // record — a second Order prompt, not completion.
        state.waiting_for = waiting_e1.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        let waiting_e2 = state.waiting_for.clone();
        expect_order_prompt(&waiting_e2, PlayerId(1), 2);
        let pending = state
            .pending_replacement
            .as_ref()
            .expect("E2's pause holds its own authority");
        assert!(
            matches!(&pending.proposed, ProposedEvent::Damage { target, .. }
                if *target == TargetRef::Object(blocker_b)),
            "E2 owns the SECOND overlap — the same batch re-paused, got {:?}",
            pending.proposed
        );
        let fresh = state.pending_combat_damage.as_ref().unwrap();
        assert_eq!(
            (fresh.paused_entry, fresh.decided.len()),
            (1, 1),
            "the fresh park re-arms the stash gate (decided.len() == paused_entry)"
        );
        assert_eq!(
            fresh.decided[0], None,
            "E1's prevention stash carries into the fresh record"
        );
        assert!(
            !fresh.paused_entry_pruned,
            "a fresh park never inherits a stale prune flag (B6 site 3)"
        );
        assert!(
            !fresh
                .batch_events
                .iter()
                .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
            "no aggregate publishes per-park: the one batch publishes once at completion"
        );

        // Answer E2: the one batch completes — each prevention and each return
        // exactly once, so no leg duplicated across the two parks.
        state.waiting_for = waiting_e2.clone();
        answer_replacement_choice(&mut state, &mut events, 1);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the batch settles after E2, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_damage.is_none());
        assert!(state.pending_replacement.is_none());
        assert!(state.combat.as_ref().unwrap().regular_damage_done);
        let prevented: Vec<(TargetRef, u32)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented { target, amount, .. } => {
                    Some((target.clone(), *amount))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            prevented.len(),
            2,
            "two preventions, neither duplicated nor dropped, got {events:?}"
        );
        assert!(
            prevented.contains(&(TargetRef::Object(blocker_a), 3))
                && prevented.contains(&(TargetRef::Object(blocker_b), 3)),
            "each overlap's prevention publishes exactly once, got {prevented:?}"
        );
        let dealt: Vec<(TargetRef, u32)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamageDealt { target, amount, .. } => Some((target.clone(), *amount)),
                _ => None,
            })
            .collect();
        assert_eq!(
            dealt.len(),
            2,
            "two returns, neither duplicated nor dropped, got {events:?}"
        );
        assert!(
            dealt.contains(&(TargetRef::Object(attacker_a), 2))
                && dealt.contains(&(TargetRef::Object(attacker_b), 2)),
            "only the two blockers' return damage is dealt, got {dealt:?}"
        );
    }

    /// CR 616.1f: ONE event needs TWO ordering choices. 10 damage vs three
    /// `Next(5)` shields: the first answer applies one shield and the SAME
    /// event re-parks (the NeedsChoice epilogue, folded by the wrapper — the
    /// record stays parked and UNSTASHED). The second answer settles the event
    /// (the stash lands once) and the batch completes as one.
    ///
    /// REVERT PROBES: stash on the repeat path → the unstashed assertion fails;
    /// drop the wrapper fold → a leg strands outside the batch (counts fail);
    /// apply the survivor per answer → the blocker's marked damage doubles.
    #[test]
    fn f1_same_event_repeat_orders_twice_then_completes_as_one() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Ogre", 10, 10);
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Giant", 1, 11);
        install_next_shield(&mut state, blocker, 5, "Ward of Embers", false);
        install_next_shield(&mut state, blocker, 5, "Ward of Thorns", false);
        install_next_shield(&mut state, blocker, 5, "Ward of Stone", false);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        // First prompt: all three shields co-applicable.
        let mut events = Vec::new();
        let waiting_first =
            resolve_combat_damage(&mut state, &mut events).expect("three shields pause");
        expect_order_prompt(&waiting_first, PlayerId(1), 3);

        // First answer: one shield applies; the SAME event re-parks instead of
        // settling — a second Order prompt, still unstashed.
        state.waiting_for = waiting_first.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        let waiting_second = state.waiting_for.clone();
        expect_order_prompt(&waiting_second, PlayerId(1), 2);
        let pending = state
            .pending_replacement
            .as_ref()
            .expect("the repeat holds its own authority");
        assert!(
            matches!(&pending.proposed, ProposedEvent::Damage { source_id, target, .. }
                if *source_id == attacker && *target == TargetRef::Object(blocker)),
            "the repeat owns the SAME event (not a nested child), got {:?}",
            pending.proposed
        );
        let record = state.pending_combat_damage.as_ref().unwrap();
        assert_eq!(
            (record.paused_entry, record.decided.len()),
            (0, 0),
            "no stash on the repeat path: the paused slot settles exactly once"
        );

        // Second answer: the event settles and the one batch completes.
        state.waiting_for = waiting_second.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the batch settles after the repeat, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_damage.is_none());
        assert!(state.pending_replacement.is_none());
        assert!(state.combat.as_ref().unwrap().regular_damage_done);
        assert_eq!(
            state.objects[&blocker].damage_marked, 0,
            "10 - 5 - 5: the survivor applies exactly once"
        );
        let mut prevented: Vec<u32> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented { amount, .. } => Some(*amount),
                _ => None,
            })
            .collect();
        prevented.sort();
        assert_eq!(
            prevented,
            vec![5, 5],
            "two shields' aggregates publish once each (a 0 prevention is not an event), got {events:?}"
        );
        assert_eq!(
            state.objects[&attacker].damage_marked, 1,
            "the blocker's return damage (remaining) still lands"
        );
    }

    /// R3 with a non-empty Phase A: a singly-protected hit claims in Phase A
    /// (tallied, gated out of the entries) while an overlap pauses in Phase B.
    /// The installs merge the parked Phase-A prefix with the answer's
    /// accumulation, so BOTH preventions publish once with correct
    /// per-(shield, recipient) amounts.
    ///
    /// REVERT PROBES: install only Phase B → the singly-protected aggregate
    /// vanishes; publish per leg → one of them duplicates.
    #[test]
    fn f1_nonempty_phase_a_tally_merges_across_pause() {
        use crate::types::keywords::{Keyword, ProtectionTarget};
        use crate::types::mana::ManaColor;

        let mut state = setup();
        // Singly-protected pair: claims in Phase A, never an entry.
        let red_attacker = create_creature(&mut state, PlayerId(0), "Red Attacker", 3, 3);
        paint(&mut state, red_attacker, vec![ManaColor::Red]);
        let guarded = create_creature(&mut state, PlayerId(1), "Guarded Bear", 2, 2);
        state
            .objects
            .get_mut(&guarded)
            .unwrap()
            .keywords
            .push(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)));
        // Overlap pair: pauses in Phase B.
        let rg_attacker = create_creature(&mut state, PlayerId(0), "RG Attacker", 3, 3);
        paint(
            &mut state,
            rg_attacker,
            vec![ManaColor::Red, ManaColor::Green],
        );
        let warded = create_creature(&mut state, PlayerId(1), "Warded Bear", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(warded),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        setup_combat(
            &mut state,
            vec![red_attacker, rg_attacker],
            vec![(red_attacker, vec![guarded]), (rg_attacker, vec![warded])],
        );

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("the overlap pauses");
        expect_order_prompt(&waiting, PlayerId(1), 2);
        let record = state.pending_combat_damage.as_ref().unwrap();
        assert_eq!(
            (record.paused_entry, record.decided.len()),
            (0, 0),
            "the gated hit is not an entry: the overlap pauses first"
        );
        assert_eq!(
            record.phase_b_tally.len(),
            0,
            "reach guard: nothing decided before the pause, so the merge's Phase-B leg is empty"
        );
        assert!(
            record
                .phase_a_tally
                .iter()
                .any(|entry| entry.recipient == TargetRef::Object(guarded) && entry.amount == 3),
            "reach guard: the singly-protected hit is tallied in Phase A at the pause, got {:?}",
            record.phase_a_tally
        );

        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the batch settles, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_damage.is_none());
        let prevented: Vec<(TargetRef, u32)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented { target, amount, .. } => {
                    Some((target.clone(), *amount))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            prevented.len(),
            2,
            "two preventions, neither duplicated nor dropped, got {events:?}"
        );
        assert!(
            prevented.contains(&(TargetRef::Object(guarded), 3))
                && prevented.contains(&(TargetRef::Object(warded), 3)),
            "both preventions publish once with correct per-recipient amounts, got {prevented:?}"
        );
        assert_eq!(state.objects[&guarded].damage_marked, 0);
        assert_eq!(state.objects[&warded].damage_marked, 0);
        assert_eq!(
            (
                state.objects[&red_attacker].damage_marked,
                state.objects[&rg_attacker].damage_marked
            ),
            (2, 2),
            "both returns (remaining) still land"
        );
    }

    /// CR 616.1f: a lifelink gain pauses INSIDE a resumed damage batch. T3's
    /// shape (lifelinker + overlap) with a gain-replacement overlap on the
    /// lifelink controller: E's answer resumes through Phase C, and the
    /// lifelink drain parks its own prompt (the `Paused` sub-arm) instead of
    /// completing. The gain's answer completes through the lifelink resume
    /// with the lifelink record drained.
    ///
    /// REVERT PROBES: complete through the damage resume without draining →
    /// no lifelink prompt (the gain vanishes); drop the parked tail → the
    /// completion asserts fail.
    #[test]
    fn f1_lifelink_pause_inside_resumed_batch() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let lifelinker = create_creature(&mut state, PlayerId(0), "Lifelink Cat", 2, 2);
        state
            .objects
            .get_mut(&lifelinker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        install_gain_life_replacement(
            &mut state,
            PlayerId(0),
            "Rhox Faithmender",
            doubler_amount(),
        );
        install_gain_life_replacement(
            &mut state,
            PlayerId(0),
            "Leyline of Hope",
            plus_one_amount(),
        );
        let attacker = create_creature(&mut state, PlayerId(0), "RG Attacker", 3, 3);
        paint(&mut state, attacker, vec![ManaColor::Red, ManaColor::Green]);
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Bear", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        setup_combat(
            &mut state,
            vec![lifelinker, attacker],
            vec![(attacker, vec![blocker])],
        );

        // E pauses on the overlap (the lifelinker is decided ahead of it).
        let mut events = Vec::new();
        let waiting_e = resolve_combat_damage(&mut state, &mut events).expect("the overlap pauses");
        expect_order_prompt(&waiting_e, PlayerId(1), 2);

        // E's answer resumes through Phase C — and the lifelink gain parks
        // its own prompt instead of completing.
        state.waiting_for = waiting_e.clone();
        answer_replacement_choice(&mut state, &mut events, 1);
        let waiting_gain = state.waiting_for.clone();
        expect_order_prompt(&waiting_gain, PlayerId(0), 2);
        let pending = state
            .pending_replacement
            .as_ref()
            .expect("the gain's pause holds its own authority");
        assert!(
            matches!(&pending.proposed, ProposedEvent::LifeGain { .. }),
            "the resumed batch parks on the LIFELINK gain, got {:?}",
            pending.proposed
        );
        assert!(
            state.pending_combat_damage.is_none(),
            "the damage record is consumed by its resume"
        );
        assert!(
            state.pending_combat_lifelink.is_some(),
            "the lifelink tail parks for its own resume"
        );

        // The gain's answer completes through the lifelink resume.
        state.waiting_for = waiting_gain.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the batch settles after the gain, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_damage.is_none());
        assert!(
            state.pending_combat_lifelink.is_none(),
            "the lifelink record drains on completion"
        );
        assert!(state.combat.as_ref().unwrap().regular_damage_done);
        let gains: Vec<i32> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::LifeChanged {
                    player_id, amount, ..
                } if *player_id == PlayerId(0) => Some(*amount),
                _ => None,
            })
            .collect();
        assert_eq!(
            gains.len(),
            1,
            "the lifelink gain applies exactly once, got {events:?}"
        );
        assert!(
            [25, 26].contains(&(20 + gains[0])),
            "doubler + plus-one order the 2 exactly once (2*2+1 or (2+1)*2), got {gains:?}"
        );
        assert_eq!(state.players[1].life, 18, "the lifelinker dealt 2");
        assert!(
            events.iter().any(
                |e| matches!(e, GameEvent::DamagePrevented { target, amount: 3, .. }
                    if *target == TargetRef::Object(blocker))
            ),
            "the overlap's prevention still publishes, got {events:?}"
        );
    }

    /// CR 510.2 + CR 615.13: the pre-batch outer window survives a mid-pause
    /// save/load. The live window is `#[serde(skip)]`, so the round trip drops
    /// it — but the park captured it in `record.outer_tally`, and the
    /// wrapper/resume installs prefer the parked copy when the live window is
    /// gone. The batch still completes with its own aggregate published once
    /// (never merged into the outer window).
    ///
    /// REVERT PROBE: install the live window alone (drop the `or_else`) → the
    /// restored-outer assertion fails (the load silently downgrades `Some` to
    /// `None`, flipping accumulate-vs-emit for later prevention).
    #[test]
    fn f1_outer_tally_restored_after_mid_pause_save_load() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;
        use crate::types::proposed_event::AppliedReplacementKey;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "RG Attacker", 3, 3);
        paint(&mut state, attacker, vec![ManaColor::Red, ManaColor::Green]);
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Bear", 2, 2);
        install_protection_overlap(
            &mut state,
            TargetRef::Object(blocker),
            ProtectionTarget::Color(ManaColor::Red),
            ProtectionTarget::Color(ManaColor::Green),
        );
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        // Seed a pre-batch outer window (an outer simultaneity's accumulation).
        let outer_key = (
            AppliedReplacementKey::Object {
                source: attacker,
                index: 0,
            },
            TargetRef::Player(PlayerId(0)),
        );
        let mut outer_window = std::collections::HashMap::new();
        outer_window.insert(outer_key, 7);
        state.combat_prevention_tally = Some(outer_window.clone());

        // The overlap parks, capturing the outer window on the record.
        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events).expect("the overlap pauses");
        expect_order_prompt(&waiting, PlayerId(1), 2);
        let record = state.pending_combat_damage.as_ref().unwrap();
        assert_eq!(
            record.outer_tally.as_ref().map(|entries| entries.len()),
            Some(1),
            "reach guard: the park captures the pre-batch window, got {:?}",
            record.outer_tally
        );
        assert_eq!(
            state.combat_prevention_tally.as_ref(),
            Some(&outer_window),
            "reach guard: the live window still holds the outer at the park"
        );

        // Mid-pause save/load: the live window drops (serde skip); the record
        // — outer tally included — survives.
        let json = serde_json::to_string(&state).expect("mid-pause state serializes");
        let mut loaded: GameState = serde_json::from_str(&json).expect("mid-pause state reloads");
        assert!(
            loaded.combat_prevention_tally.is_none(),
            "the hole: the live window does not survive the round trip"
        );
        assert_eq!(
            loaded.pending_combat_damage.as_ref().unwrap().outer_tally,
            record.outer_tally,
            "the parked outer tally survives the round trip"
        );

        // Answer on the reloaded state: the batch completes AND the outer
        // window is restored exactly (not merged, not dropped).
        loaded.waiting_for = waiting.clone();
        let mut loaded_events = Vec::new();
        answer_replacement_choice(&mut loaded, &mut loaded_events, 0);
        assert!(
            matches!(loaded.waiting_for, WaitingFor::Priority { .. }),
            "the reloaded batch settles, got {:?}",
            loaded.waiting_for
        );
        assert!(loaded.pending_combat_damage.is_none());
        assert_eq!(
            loaded.combat_prevention_tally.as_ref(),
            Some(&outer_window),
            "the pre-batch outer window is restored on completion"
        );
        let prevented: Vec<(TargetRef, u32)> = loaded_events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamagePrevented { target, amount, .. } => {
                    Some((target.clone(), *amount))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            prevented,
            vec![(TargetRef::Object(blocker), 3)],
            "the batch publishes its own aggregate once — never merged into the outer, got {loaded_events:?}"
        );
    }
}
