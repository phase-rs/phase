use crate::game::combat::{AttackTarget, CombatState, DamageAssignment, DamageTarget, TrampleKind};
use crate::game::effects::deal_damage::{
    apply_damage_after_replacement, pre_replacement_damage_gate,
    settle_parked_player_damage_results, tally_map_from_record_entries,
    wither_infect_post_counter_core, DamageContext, DamageGateOutcome, DamagePostLossDrainOutcome,
    DamageResult, PostCounterFlow,
};
use crate::game::effects::life::{ReplacementDeferred, StagedLifeGain};
use crate::game::game_object::GameObject;
use crate::game::game_object::ObjectIncarnationRefExt;
use crate::game::lethal_damage::{self, AssignedThisStep};
use crate::game::replacement;
use crate::game::sba;
use crate::game::triggers;
use crate::game::turns;
use crate::types::ability::{DamageContextSnapshot, ShieldKind, TargetRef};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CombatDamageAssignmentMode, CombatDamageSubStep, CombatPreventionTally, DamageSlot, GameState,
    PendingCombatDamageEntry, PendingCombatDamageReplacement, PendingCombatLeg,
    PendingCombatLegProgress, PendingCombatLifelink, PendingCombatPhaseCRemnant,
    PendingLifelinkGain, PerPlayerCombatDamage, PreventedDamageEntry, WaitingFor,
};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{AppliedReplacementKey, ProposedEvent, ReplacementId};
use std::collections::VecDeque;

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
    let mut pending =
        triggers::collect_combat_damage_batch_triggers(state, &before_priority_events);

    // Steps 2-4: SBA/trigger loop per CR 704.3.
    // SBAs may generate events (ZoneChanged for dying creatures) that need trigger
    // processing (dies triggers). Repeat until no new SBAs and no new triggers.
    loop {
        let events_before = all_events.len();
        // CR 704.3 + CR 510.3a + CR 704.5v: SBAs run before this batch is put
        // on the stack, but its abilities have already triggered — a Siege
        // whose CR 310.12b victory trigger waits here is not put into its
        // owner's graveyard.
        let departure_queued =
            sba::check_state_based_actions_with_waiting_triggers(state, all_events, &pending);

        // If SBAs generated new events, process triggers for those events.
        if all_events.len() > events_before {
            // CR 603.10a + CR 603.2c: a player's departure in that SBA event was
            // collected, once, against the observers as they were immediately
            // before it, and its occurrences claimed: its triggers join this
            // batch and the scans below skip the claimed events.
            let departure_triggers = state.deferred_triggers.split_off(
                state
                    .deferred_triggers
                    .len()
                    .saturating_sub(departure_queued),
            );
            pending.extend(departure_triggers);
            let consumed = state.consumed_before_priority_trigger_events.clone();
            let new_events = triggers::filter_already_collected_trigger_events_from(
                state,
                all_events,
                events_before,
                &consumed,
            );
            before_priority_events.extend(triggers::filter_consumed_trigger_events_from(
                all_events,
                events_before,
                triggers::TriggerCollectionRequester::Delayed,
                &consumed,
            ));
            pending.extend(triggers::collect_triggers_for_batch(state, &new_events));
            // CR 603.2c: this loop also owns the SBA results it just scanned.
            // Do not let an open trigger-ordering prompt make the generic
            // action collector observe the same deaths again.
            claim_combat_batch_events_for_ordinary_collection(state, all_events, events_before);
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
/// Between sub-steps: SBAs are checked, triggers processed, and (CR 510.3) the active
/// player gets priority.
///
/// Performs at most ONE combat damage step per call. After the first-strike step it
/// returns `Some(WaitingFor::Priority)` (the CR 510.3 window, whether or not anything is
/// on the stack); the all-pass completeness gate in `priority.rs` calls it again for the
/// regular step.
///
/// Returns `Some(WaitingFor)` when an attacker with 2+ blockers needs interactive
/// damage assignment, or at the window after the first-strike step. Returns `None` when
/// the regular step's damage is resolved.
/// Re-entrant: call again after the player submits `GameAction::AssignCombatDamage`.
pub fn resolve_combat_damage(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    // CR 510.2 + CR 616.1: a parked batch still owns this turn-based action and
    // must never be replayed (the batch is one event). For a lifelink-gain
    // pause its damage is ALREADY dealt; for a Phase C leg pause the remnant
    // resume completes the unapplied legs. Placed AHEAD of
    // `state.combat.as_ref()?` so no combat teardown can strand the record
    // behind that `?`.
    if state.combat_damage_batch_parked() {
        // Any live prompt — the CR 616.1 ordering choice itself, or a sub-choice
        // raised while answering it — means an answer is still coming. Surface it
        // rather than re-collecting assignments. The `matches!` below excludes
        // `Priority`, so what is returned here is always a genuine prompt; a bare
        // `Priority` comes only from the CR 510.3 window after the first-strike
        // step, never from a parked batch.
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return Some(state.waiting_for.clone());
        }
        // Settled to priority with a batch still parked. An answer that settles to
        // priority finishes the batch in its own action
        // (`engine::resume_parked_combat_damage_at_action_boundary`), also after the
        // active player left the game (CR 800.4j: the completed batch's window goes
        // to `engine::living_priority_seat`). So this is reached, from priority.rs's
        // completeness gate, only when that convergence held back the batch's next
        // prompt because it names a seat that left the game (CR 800.4a, the same
        // aliveness test `turns::auto_advance_once` applies), or from a save written
        // before that convergence existed. No further answer will
        // arrive, so finish the batch here — otherwise the owed gains die with
        // combat AND the `!regular_damage_done` completeness gate (priority.rs)
        // re-enters this function forever.
        //
        // `events.len()` folds nothing: no answering action's events are in
        // flight on this path.
        return resume_pending_combat_lifelink(state, events.len(), None, events);
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
        claim_combat_batch_events_for_ordinary_collection(state, events, batch_event_start);
        match batch {
            // CR 616.1: the batch parked on a life-gain ordering choice. The
            // sub-step flags are deliberately NOT set here — the resume sets them
            // once the batch actually completes, so neither re-entry nor
            // priority.rs's completeness gate can skip the rest of combat damage.
            CombatDamageBatch::Paused {
                events: _,
                waiting_for,
            } => return Some(*waiting_for),
            // CR 510.3 + CR 510.4: the first combat damage step is complete and ends
            // with the active player's priority window (or a prompt that must come
            // first); the regular step is a separate call, entered once that window
            // closes.
            CombatDamageBatch::Complete(damage_events) => {
                return finish_combat_damage_sub_step(
                    state,
                    CombatDamageSubStep::FirstStrike,
                    &damage_events,
                    events,
                );
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
    claim_combat_batch_events_for_ordinary_collection(state, events, batch_event_start);
    match batch {
        // CR 616.1: see the first-strike arm above — the completion flags belong
        // to the resume, not to a batch that has parked.
        CombatDamageBatch::Paused {
            events: _,
            waiting_for,
        } => Some(*waiting_for),
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
    // CR 104.1: a rider ended the game inside the batch ("A game ends immediately when a player
    // wins"), so nothing after the batch happens: no sub-step bookkeeping, no state-based
    // actions, no ability put on the stack, and no later combat damage step. A rider's drain
    // restored the caller's wait over the batch's `GameOver`, so restore it from `game_end`.
    if state.game_end.is_some() {
        crate::game::elimination::ensure_game_over_if_terminal(state, events);
        return pending_combat_damage_waiting(state);
    }

    // CR 500.6: "at the beginning of that step" abilities trigger when the step
    // begins. `process_combat_damage_triggers` synthesizes the PhaseChanged
    // marker for that batch, so the FIRST-STRIKE sub-step always carries it, and
    // the regular sub-step carries it only when it is the ONLY sub-step.
    //
    // Reproduces the previous `!combat.first_strike_done && !has_first_or_double`,
    // which read the entry-time clone. Evaluated BEFORE the `regular_damage_done`
    // write. Equivalent for both reachable entry states (one combat damage step per
    // call, so the first-strike step is never followed by the regular step in the
    // same call):
    //   * no first strike, single sub-step               -> true  (== old true)
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
            // complete step, and every combat damage step ends the same way: abilities
            // that triggered on the damage (or on SBAs taken afterward) are put on the
            // stack, and THEN the active player receives priority. That window is
            // unconditional, so players can respond before the second (regular) combat-
            // damage step begins even when nothing is on the stack (kill, pump or give
            // double strike to a creature, activate an ability, ninjutsu).
            // A trigger on the stack (e.g. No Mercy's "destroy that creature", a damage
            // trigger that bounces/exiles the attacker) is one case, resolved inside the
            // window before a now-doomed double-strike attacker can deal its regular-
            // sub-step damage (#692); an empty stack is the other.
            // Returning here leaves `regular_damage_done == false`; the mandatory regular
            // sub-step is resumed once the stack is empty and all players pass, via the
            // empty-stack completeness gate in priority.rs.
            //
            // reset_priority here is defensive — unlike the sibling regular-substep entry in
            // turns.rs, this returns mid-step after the first-strike substep, so we explicitly
            // clear any stale passes before the CR 510.3 priority window (harmless if already clear).
            crate::game::priority::reset_priority(state);
            Some(WaitingFor::Priority {
                player: state.active_player,
            })
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
/// it as a local before the answer is applied, the action boundary takes it from
/// its [`ParkedCombatDamageAnswer`], and the re-entry guard passes `events.len()`
/// when no answer is in flight. It is clamped to `events.len()` at entry, so the
/// fold and the claims below read one range that is never out of bounds.
///
/// The answering action's events — the resumed `LifeChanged` among them — are
/// folded into the parked batch by the one fold rule,
/// [`take_answer_events_into_batch`], so "whenever you gain life" observers
/// (CR 119.9) join the SAME CR 603.3b batch as the combat-damage observers.
/// CR 702.15b makes the gain simultaneous with the damage, and no player
/// received priority for the CR 616.1 choice, so splitting them would be a rules
/// regression.
///
/// CR 616.1 + CR 616.1g: a batch parked EARLIER, in its replacement pass
/// (`PendingCombatLifelink::replacement`), resumes through the same authority.
/// `choice` is the answer to that pass's own ordering choice, consumed here
/// rather than by the generic dispatcher (see
/// [`parked_combat_damage_awaits_replacement_choice`]); a substitute's prompt
/// (counters, mill) was answered by its own dispatcher before this runs, and
/// its events fold into the batch exactly like the lifelink answer's. The pass
/// then finishes, and Phase C and the lifelink tail run once.
///
/// Returns `None` when nothing is parked, so callers can keep their own wait.
pub(crate) fn resume_pending_combat_lifelink(
    state: &mut GameState,
    action_event_start: usize,
    choice: Option<usize>,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let action_event_start = action_event_start.min(events.len());
    let mut record = *state.pending_combat_lifelink.take()?;
    // Every pause below re-sets the marker when it re-parks on a gain's own choice; an
    // answer that resolved without reaching `take_staged_answer` (a Prevented
    // or substituted gain) must not leave it standing.
    record.awaiting_staged_gain = false;
    take_answer_events_into_batch(state, &mut record, events, action_event_start);
    let owed_from = record.batch_events.len();
    let sub_step = record.sub_step;
    // CR 510.2 + CR 616.1: a batch parked in Phase C completes its Phase C tail
    // before draining lifelink. The answer has settled the leg's result or the
    // early gain's choice. Any new pause parks again with the batch so far.
    if let Some(remnant) = record.phase_c_remnant.take() {
        match complete_combat_phase_c_remnant(state, remnant, &mut record) {
            RemnantOutcome::Complete => {}
            RemnantOutcome::Repaused { remnant, on_gain } => {
                record.phase_c_remnant = Some(remnant);
                record.awaiting_staged_gain = on_gain;
                let waiting_for = state.waiting_for.clone();
                let batch = record.batch_events.clone();
                state.pending_combat_lifelink = Some(Box::new(record));
                events.extend_from_slice(&batch[owed_from..]);
                claim_combat_batch_events_for_ordinary_collection(
                    state,
                    events,
                    action_event_start,
                );
                return Some(waiting_for);
            }
        }
    }
    match resume_combat_damage_replacements(state, record, choice) {
        CombatDamageBatch::Paused {
            events: batch,
            waiting_for,
        } => {
            // CR 616.1f + CR 616.1g: a LATER source (or the same event's
            // substitute, or a second ordering choice on it) needed its own
            // answer. The still-untouched rest was re-parked with the batch so
            // far, so the eventual completion still fires ONE CR 603.3b batch.
            events.extend_from_slice(&batch[owed_from..]);
            claim_combat_batch_events_for_ordinary_collection(state, events, action_event_start);
            Some(*waiting_for)
        }
        CombatDamageBatch::Complete(damage_events) => {
            events.extend_from_slice(&damage_events[owed_from..]);
            let waiting_for =
                finish_combat_damage_sub_step(state, sub_step, &damage_events, events);
            claim_combat_batch_events_for_ordinary_collection(state, events, action_event_start);
            // CR 510.3 + CR 510.4: a completed batch ends its combat damage step with
            // the active player's priority window. A first-strike batch already
            // returned that window (or a prompt that must come first) from
            // `finish_combat_damage_sub_step`, and the mandatory regular step is
            // entered through it by the all-pass gate in priority.rs, the one path
            // the inline first-strike step takes too; a regular batch is the last
            // step and ends with this window.
            //
            // CR 800.4j: a batch answered after the active player left the game
            // (a surviving chooser's answer to its parked choice) gives that window
            // to the next player in turn order still in the game, never to the seat
            // that left.
            match waiting_for {
                None | Some(WaitingFor::Priority { .. }) => Some(WaitingFor::Priority {
                    player: super::engine::living_priority_seat(state),
                }),
                prompt => prompt,
            }
        }
    }
}

/// CR 101.4 + CR 616.1: the answered final event of a combat lifelink gain
/// being staged. When the parked batch awaits a staged gain, the event is
/// recorded on the batch (the Phase C remnant's staged gains while the
/// remnant is parked, else the record's) and NOT settled: it settles with
/// the batch's other gains once every choice is made. Returns `None` when
/// claimed; otherwise hands the event back for the caller to settle.
pub(crate) fn take_staged_answer(
    state: &mut GameState,
    gain: ProposedEvent,
) -> Option<ProposedEvent> {
    let ProposedEvent::LifeGain {
        player_id, amount, ..
    } = &gain
    else {
        return Some(gain);
    };
    let Some(record) = state
        .pending_combat_lifelink
        .as_mut()
        .filter(|record| record.awaiting_staged_gain)
    else {
        return Some(gain);
    };
    record.awaiting_staged_gain = false;
    let staged = PendingLifelinkGain {
        controller: *player_id,
        amount: *amount,
    };
    match record.phase_c_remnant.as_mut() {
        Some(remnant) => remnant.staged_gains.push(staged),
        None => record.staged_gains.push(staged),
    }
    None
}

/// CR 510.2 + CR 603.2c: the damage batch owns ordinary collection even
/// when it returns with an ordering prompt and nothing has reached the stack.
/// Claim its exact public occurrences only against the generic ordinary scan;
/// the batch's own resumed and delayed collectors can still see them.
pub(crate) fn claim_combat_batch_events_for_ordinary_collection(
    state: &mut GameState,
    events: &[GameEvent],
    event_start: usize,
) {
    triggers::claim_events_for_ordinary_collection(state, events, event_start);
}

/// CR 510.2: an action that answers a parked combat damage batch's choice, and
/// where in its event buffer the answer's events begin.
///
/// Only an action that found the batch parked as it began is an answer. An
/// action that parks the batch (declaring blockers, the step's start) publishes
/// and claims its own events as it parks, so it never folds them a second time.
/// Taken as a local at an action boundary and never stored, so no index outlives
/// its action.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ParkedCombatDamageAnswer {
    action_event_start: usize,
}

impl ParkedCombatDamageAnswer {
    /// `Some` only when a combat damage batch is parked as the action begins;
    /// `action_event_start` is the length of the action's event buffer then.
    pub(crate) fn begin(state: &GameState, action_event_start: usize) -> Option<Self> {
        state
            .combat_damage_batch_parked()
            .then_some(Self { action_event_start })
    }

    pub(crate) fn action_event_start(self) -> usize {
        self.action_event_start
    }
}

/// CR 510.2 + CR 603.2 + CR 603.2c + CR 603.3b: the one fold rule — a parked
/// combat damage batch takes custody of its answering action's events, so each
/// occurrence is collected once, by the batch's one placement when it completes.
///
/// Three kinds of occurrence are left out, each because another owner collects
/// it, as the post-action pipeline leaves them out too:
/// * an occurrence already claimed for ordinary collection, except a queued
///   zone-change witness the batch has yet to take;
/// * a parked battlefield entry, whose replay collects it (issue #830);
/// * an occurrence a paused logical-zone or cost owner retains for its own
///   settlement.
///
/// Keep a queued context's `ZoneChanged` witness even when a staged zone
/// delivery claimed it for ordinary collection. The batch uses it to move
/// the queued context into its placement instead of rescanning it. A claim
/// covering delayed collectors still excludes the occurrence (CR 603.7b).
/// Known gap: an occurrence claimed for all collectors while the batch is
/// parked (a concession's departure, CR 800.4a) has no witness here, so its
/// queued context is placed after the batch's placement, not in it
/// (Command Zone #353).
///
/// The start is clamped: an anchor taken before the answer can exceed a buffer
/// the answer shortened (`collect_answer_slice_matching` documents this for
/// `zone_pipeline::drain_pending_batch_deliveries`).
fn take_answer_events_into_batch(
    state: &GameState,
    record: &mut PendingCombatLifelink,
    events: &[GameEvent],
    action_event_start: usize,
) {
    let start = action_event_start.min(events.len());
    let mut retained = crate::game::engine_priority::retained_logical_zone_events(state);
    retained.extend(crate::game::engine_priority::retained_cost_events(state));
    let consumed = &state.consumed_before_priority_trigger_events;
    let ordinary = triggers::unclaimed_trigger_events_from(
        events,
        start,
        triggers::TriggerCollectionRequester::Ordinary,
        consumed,
    );
    let batch = triggers::unclaimed_trigger_events_from(
        events,
        start,
        triggers::TriggerCollectionRequester::CombatDamageBatch,
        consumed,
    );
    for (&position, event) in batch.positions().iter().zip(&batch.events) {
        if crate::game::engine_replacement::entry_event_is_parked(state, event)
            || retained.contains(event)
        {
            continue;
        }
        let queued_zone_witness = matches!(event, GameEvent::ZoneChanged { .. })
            && !record.batch_events.contains(event)
            && state.deferred_triggers.iter().any(|context| {
                context.batch_origin == triggers::DeferredTriggerBatchOrigin::Ordinary
                    && context.trigger_events.contains(event)
            });
        if ordinary.positions().binary_search(&position).is_ok() || queued_zone_witness {
            record.batch_events.push(event.clone());
        }
    }
}

/// CR 603.2 + CR 603.3 + CR 510.3a: an answer that leaves the combat damage
/// batch parked (its next choice is asked) hands the batch its events, so their
/// triggers join the combat damage step's one placement once, when the batch
/// completes. The events are claimed for ordinary collection, so no generic
/// collector takes them in the meantime.
pub(crate) fn hold_parked_combat_damage_answer_events(
    state: &mut GameState,
    answer: ParkedCombatDamageAnswer,
    events: &[GameEvent],
) {
    let Some(mut record) = state.pending_combat_lifelink.take() else {
        return;
    };
    let start = answer.action_event_start().min(events.len());
    take_answer_events_into_batch(state, &mut record, events, start);
    state.pending_combat_lifelink = Some(record);
    claim_combat_batch_events_for_ordinary_collection(state, events, start);
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

/// Whether combat sits between its two combat damage steps with nothing on the
/// stack: the first-strike step is over, the regular step has not begun, and the
/// active player's CR 510.3 priority window is open (or about to be passed).
///
/// CR 510.4: a combat with a first-strike or double-strike creature has two
/// combat damage steps. CR 510.3: each step ends with the active player getting
/// priority, so passing it on an empty stack (CR 117.4) is what starts the
/// second step. Reads only the combat flags and the stack, never `waiting_for`
/// or `phase`, so the unit-test drivers that call [`resolve_combat_damage`]
/// directly share it with the integration harness.
pub(crate) fn between_combat_damage_steps(state: &GameState) -> bool {
    state
        .combat
        .as_ref()
        .is_some_and(|combat| combat.first_strike_done && !combat.regular_damage_done)
        && state.stack.is_empty()
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
fn survives_marked_damage(
    state: &GameState,
    object: &GameObject,
    incoming_damage: u32,
) -> Option<bool> {
    // CR 704.5f + CR 704.5g: a creature with toughness 0 or less dies regardless of damage;
    // otherwise it survives unless its marked plus incoming damage is lethal (CR 120.6, whose
    // basis may be power under CR 613.11).
    let toughness = object.toughness?;
    Some(
        toughness > 0
            && !lethal_damage::is_lethal_damage(
                state,
                object,
                object.damage_marked.saturating_add(incoming_damage),
            ),
    )
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
        survives_marked_damage(state, attacker_object, damage.blocker_to_attacker),
        survives_marked_damage(state, blocker_object, damage.attacker_to_blocker),
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
                    // CR 702.19b: counts damage earlier attackers committed this step; see
                    // `combat_lethal_damage_remaining` for the CR 510.1e restriction.
                    lethal_minimum: lethal_damage::combat_lethal_damage_remaining(
                        state,
                        bid,
                        has_deathtouch,
                        assigned_this_step(state, bid),
                    ),
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
                compute_pw_loyalty_threshold(state, attacker_info)
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
            return power
                > lethal_damage::combat_lethal_damage_remaining(
                    state,
                    blocker_id,
                    has_deathtouch,
                    assigned_this_step(state, blocker_id),
                );
        }
    }

    blocker_count >= 2
}

/// CR 702.19b: combat damage other attackers have already committed to `blocker_id` earlier in
/// this combat damage step, split by whether the assigning source has deathtouch (CR 702.2c).
///
/// Reads the live `state.combat`, never a copy taken before the attacker loop, so it sees both
/// earlier auto-assignments and an answered `AssignCombatDamage` prompt. Mirrors
/// `compute_pw_loyalty_threshold` (CR 702.19c).
///
/// CR 510.1e (partial): only damage already committed is counted. An earlier attacker cannot
/// assign excess based on damage that a later attacker will assign. Tracked by Command Zone #285.
fn assigned_this_step(state: &GameState, blocker_id: ObjectId) -> AssignedThisStep {
    let Some(combat) = state.combat.as_ref() else {
        return AssignedThisStep::default();
    };
    combat
        .pending_damage
        .iter()
        .filter(|(_, da)| da.target == DamageTarget::Object(blocker_id))
        .fold(AssignedThisStep::default(), |mut acc, (source, da)| {
            let source_has_deathtouch = state
                .objects
                .get(source)
                .is_some_and(|obj| obj.has_keyword(&Keyword::Deathtouch));
            if source_has_deathtouch {
                acc.from_deathtouch += da.amount;
            } else {
                acc.from_other += da.amount;
            }
            acc
        })
}

/// CR 702.19c: Compute effective PW loyalty threshold for trample-over-PW,
/// accounting for pending damage from other attackers in the same step.
pub(crate) fn compute_pw_loyalty_threshold(
    state: &GameState,
    attacker_info: &crate::game::combat::AttackerInfo,
) -> (Option<u32>, Option<PlayerId>) {
    if let AttackTarget::Planeswalker(pw_id) = attacker_info.attack_target {
        // CR 506.4c + CR 702.19e: an ended relation has no current loyalty
        // threshold; only the original defender remains eligible for spillover.
        if crate::game::combat::active_attack_target(state, attacker_info).is_none() {
            return (None, Some(attacker_info.defending_player));
        }
        // CR 306.8: PW loyalty is tracked via the `loyalty` field (authoritative),
        // synced with counters on damage application. Read the field directly.
        let base_loyalty = state
            .objects
            .get(&pw_id)
            .and_then(|obj| obj.loyalty)
            .unwrap_or(0);
        // CR 702.19c: Account for pending damage from other attackers this step
        let pending_to_pw: u32 = state
            .combat
            .as_ref()
            .map(|c| {
                c.pending_damage
                    .iter()
                    .filter(|(_, da)| da.target == DamageTarget::Object(pw_id))
                    .map(|(_, da)| da.amount)
                    .sum()
            })
            .unwrap_or(0);
        let effective = base_loyalty.saturating_sub(pending_to_pw);
        let controller = state.objects.get(&pw_id).map(|obj| obj.controller);
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
    let (pw_loyalty, pw_controller) = compute_pw_loyalty_threshold(state, attacker_info);
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
            target: DamageTarget::Player(pw_controller.unwrap_or(attacker_info.defending_player)),
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
                    let lethal = lethal_damage::combat_lethal_damage_remaining(
                        state,
                        blockers[0],
                        has_deathtouch,
                        assigned_this_step(state, blockers[0]),
                    );
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
///   them. "Lethal" counts damage already marked (via
///   `lethal_damage::combat_lethal_damage_remaining`).
/// - CR 702.19d: a blocked trampler with no blockers left when damage is assigned
///   assigns everything to the player, as though all blockers had been assigned
///   lethal damage. This is what makes the second strike of a double striker so
///   dangerous: the first strike step usually empties the block.
/// - CR 702.4b + CR 702.7b: first and double strike split combat into two damage
///   steps. Blockers killed in the first step are gone for the second, and a
///   first-striking blocker can kill the attacker before its regular-step damage.
/// - CR 704.3 + CR 611.3b: the deaths between the steps are the repeated
///   state-based-action check. A blocker that dies stops generating its static
///   effects, so when it made lethal damage be measured by power (Zilortha,
///   Strength Incarnate), a survivor's first-step damage is checked again against
///   its toughness, and the regular step owes each survivor its lethal damage on
///   that battlefield.
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
    let power = combat_damage_amount(attacker);
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
        return if has_double_strike { power * 2 } else { power } as i32;
    }

    // CR 702.7b: which blockers assign their damage in the first-strike step.
    let first_strike_blockers: Vec<&GameObject> = blockers
        .iter()
        .filter_map(|bid| state.objects.get(bid))
        .filter(|b| b.has_keyword(&Keyword::FirstStrike) || b.has_keyword(&Keyword::DoubleStrike))
        .collect();

    let mut to_player = 0;
    // CR 510.2: the damage marked during the first-strike step carries into the
    // regular step (CR 120.6), after state-based actions remove any destroyed creatures.
    let mut first_step_hits = Vec::new();
    let mut departed: Vec<ObjectId> = Vec::new();
    let mut preview = None;

    // CR 702.4b: the first combat damage step exists at all only if some
    // participant has first or double strike.
    if attacker_strikes_first || !first_strike_blockers.is_empty() {
        let mut assignments = Vec::new();
        if attacker_strikes_first {
            // A hypothetical single-attacker exchange: no other attacker has committed damage.
            let slots = owed_slots(blockers.iter().map(|&bid| {
                (
                    bid,
                    lethal_damage::combat_lethal_damage_remaining(
                        state,
                        bid,
                        has_deathtouch,
                        AssignedThisStep::default(),
                    ),
                )
            }));
            let step = assign_step_to_defender(&slots, power, has_trample);
            to_player += step.to_player;
            assignments.extend(step.to_blockers.into_iter().map(|(id, amount)| {
                (
                    attacker_id,
                    DamageAssignment {
                        target: DamageTarget::Object(id),
                        amount,
                    },
                )
            }));
        }
        // CR 702.7b: first-striking blockers assign now, and can kill the attacker
        // before it ever reaches the regular step.
        // CR 510.1a + CR 613.11: same authority for the blockers — a first-striking
        // Doran-style blocker kills the attacker with its toughness, and one that
        // assigns no combat damage cannot kill it at all.
        assignments.extend(first_strike_blockers.iter().map(|blocker| {
            (
                blocker.id,
                DamageAssignment {
                    target: DamageTarget::Object(attacker_id),
                    amount: combat_damage_amount(blocker),
                },
            )
        }));
        preview = preview_first_step(state, &assignments);
        if preview.is_none() {
            // The creatures this step can damage: the blockers when the attacker strikes
            // first, and the attacker itself.
            let struck = if attacker_strikes_first {
                blockers
            } else {
                &[]
            };
            first_step_hits = struck
                .iter()
                .copied()
                .chain(std::iter::once(attacker_id))
                .map(|id| PredictedHit {
                    id,
                    amount: 0,
                    deathtouch: false,
                })
                .collect();
            forecast_assigned_hits(state, &assignments, &mut first_step_hits);
            departed = first_step_departures(state, &first_step_hits);
        }
    }

    let has_departed = |id| match &preview {
        Some(preview) => state
            .objects
            .get(&id)
            .is_some_and(|obj| !ObjectIncarnationRef::from_object(obj).is_current(preview)),
        None => departed.contains(&id),
    };
    let regular_state = preview.as_ref().unwrap_or(state);
    // CR 702.4b: the attacker assigns in the regular step if it has double strike,
    // or if it never had first strike and so has not assigned at all yet.
    if !has_departed(attacker_id) && (has_double_strike || !attacker_strikes_first) {
        // CR 506.4: a destroyed blocker has left combat. Each survivor is owed lethal damage
        // counting what the first step marked on it (CR 702.19b), measured on the battlefield
        // the first step left: a departed creature's static no longer sets the basis
        // (CR 611.3b). With no first-strike step this is the live remainder.
        let slots = owed_slots(
            blockers
                .iter()
                .filter(|&&bid| !has_departed(bid))
                .map(|&bid| {
                    let first_step = first_step_hits
                        .iter()
                        .find(|hit| hit.id == bid)
                        .map_or(0, |hit| hit.amount);
                    let owed = regular_state.objects.get(&bid).map_or(1, |obj| {
                        lethal_damage::lethal_damage_still_owed(
                            regular_state,
                            obj,
                            obj.damage_marked.saturating_add(first_step),
                            has_deathtouch,
                            &departed,
                        )
                    });
                    (bid, owed)
                }),
        );
        to_player += assign_step_to_defender(&slots, power, has_trample).to_player;
    }

    to_player as i32
}

/// CR 510.2 + CR 120.3b + CR 120.3g + CR 702.70a: the board after each unblocked attacker deals one step of combat damage to its player and the selected triggers that damage fires resolve, on a copy; None when either stops for a choice.
pub fn preview_unblocked_combat_damage(
    state: &GameState,
    attacks: &[(ObjectId, PlayerId)],
    resolves: impl Fn(&crate::types::ability::ResolvedAbility) -> bool,
) -> Option<GameState> {
    let mut preview = state.clone();
    let assignments: Vec<(ObjectId, DamageAssignment)> = attacks
        .iter()
        .filter_map(|&(attacker, player)| {
            let amount = combat_damage_amount(preview.objects.get(&attacker)?);
            Some((
                attacker,
                DamageAssignment {
                    target: DamageTarget::Player(player),
                    amount,
                },
            ))
        })
        .collect();
    let damage_events =
        match apply_combat_damage(&mut preview, &assignments, CombatDamageSubStep::Regular) {
            CombatDamageBatch::Complete(events) => events,
            CombatDamageBatch::Paused { .. } => return None,
        };
    let waiting_for = preview.waiting_for.clone();
    let mut events = Vec::new();
    for fired in triggers::collect_triggers_for_batch(&mut preview, &damage_events) {
        let source = fired.pending.source_id;
        if !(attacks.iter().any(|&(attacker, _)| attacker == source)
            && resolves(&fired.pending.ability))
        {
            continue;
        }
        // CR 603.3 + CR 702.70a: the trigger's controller gives its counters when it resolves.
        triggers::push_pending_trigger_to_stack(&mut preview, fired.pending, &mut events);
        crate::game::stack::resolve_top(&mut preview, &mut events);
        if preview.waiting_for != waiting_for {
            return None;
        }
    }
    Some(preview)
}

/// One combat damage step's worth of assignment from a single attacker: the damage that
/// reaches the defending player, and the damage assigned to each blocker.
struct StepAssignment {
    to_player: u32,
    to_blockers: Vec<(ObjectId, u32)>,
}

/// Damage a forecast combat damage step deals one creature: `amount`, and whether its source
/// has deathtouch (CR 702.2b: only nonzero damage counts).
struct PredictedHit {
    id: ObjectId,
    amount: u32,
    deathtouch: bool,
}

/// CR 510.2 + CR 615.5: when some first-step damage is not fixed, run the step on a copy of
/// the game, prevention riders included (protection granted after blocks, a Protean Hydra
/// losing counters), then its state-based actions, and return that copy for the regular step
/// to read. `None` when the damage is fixed or the copy pauses for a player's choice; the
/// caller then estimates from the assigned amounts.
fn preview_first_step(
    state: &GameState,
    assignments: &[(ObjectId, DamageAssignment)],
) -> Option<GameState> {
    let needs_preview = assignments.iter().any(|(source, assignment)| {
        let DamageTarget::Object(target) = assignment.target else {
            return false;
        };
        DamageContext::from_source(state, *source).is_some_and(|context| {
            damage_is_not_fixed(
                state,
                &context,
                &TargetRef::Object(target),
                assignment.amount,
            )
        })
    });
    if !needs_preview {
        return None;
    }
    let mut preview = state.clone();
    if let CombatDamageBatch::Paused { .. } =
        apply_combat_damage(&mut preview, assignments, CombatDamageSubStep::FirstStrike)
    {
        return None;
    }

    // CR 704.3 + CR 704.5f + CR 704.5g + CR 704.5h: check for zero toughness and
    // lethal damage on the battlefield left by first strike, repeating after departures.
    sba::check_state_based_actions(&mut preview, &mut Vec::new());
    Some(preview)
}

/// Fills `hits` with the first-step damage as assigned, for a step whose damage is fixed (no
/// prevention or replacement applies) or whose preview paused for a player's choice; a source
/// with deathtouch marks nonzero damage as deathtouch damage (CR 702.2b).
fn forecast_assigned_hits(
    state: &GameState,
    assignments: &[(ObjectId, DamageAssignment)],
    hits: &mut [PredictedHit],
) {
    for (source, assignment) in assignments {
        let DamageTarget::Object(target) = assignment.target else {
            continue;
        };
        if let Some(hit) = hits.iter_mut().find(|hit| hit.id == target) {
            hit.amount = hit.amount.saturating_add(assignment.amount);
            hit.deathtouch |= assignment.amount > 0
                && state
                    .objects
                    .get(source)
                    .is_some_and(|object| object.has_keyword(&Keyword::Deathtouch));
        }
    }
}

/// The blockers with the lethal damage owed to each, cheapest kill first. The sort is stable,
/// so equally cheap blockers keep their block order.
fn owed_slots(slots: impl Iterator<Item = (ObjectId, u32)>) -> Vec<(ObjectId, u32)> {
    let mut slots: Vec<(ObjectId, u32)> = slots.collect();
    slots.sort_by_key(|&(_, owed)| owed);
    slots
}

/// CR 704.3 + CR 704.5g + CR 704.5h + CR 611.3b: the creatures the state-based actions after a
/// forecast combat damage step destroy, given the damage that step deals (`hits`).
///
/// The check repeats while it destroys something (CR 704.3). A destroyed creature's static
/// stops applying once it leaves the battlefield (CR 611.3b), so when it was the one making
/// lethal damage be measured by power (Zilortha, Strength Incarnate), damage that was not
/// lethal against a survivor's power can be lethal against its toughness at the next check.
/// `lethal_damage::is_destroyed_by_lethal_damage` answers each check, on the battlefield the
/// earlier checks left.
fn first_step_departures(state: &GameState, hits: &[PredictedHit]) -> Vec<ObjectId> {
    let mut departed: Vec<ObjectId> = Vec::new();
    loop {
        let destroyed: Vec<ObjectId> = hits
            .iter()
            .filter(|hit| !departed.contains(&hit.id))
            .filter(|hit| {
                state.objects.get(&hit.id).is_some_and(|obj| {
                    lethal_damage::is_destroyed_by_lethal_damage(
                        state,
                        obj,
                        obj.damage_marked.saturating_add(hit.amount),
                        obj.dealt_deathtouch_damage || hit.deathtouch,
                        &departed,
                    )
                })
            })
            .map(|hit| hit.id)
            .collect();
        if destroyed.is_empty() {
            return departed;
        }
        departed.extend(destroyed);
    }
}

/// One combat damage step's worth of assignment from a single attacker against `slots`, the
/// blockers in the order `owed_slots` gives. See `combat_damage_to_defender` for the governing
/// rules.
fn assign_step_to_defender(
    slots: &[(ObjectId, u32)],
    power: u32,
    has_trample: bool,
) -> StepAssignment {
    if slots.is_empty() {
        // CR 702.19d: blocked, but nothing is blocking it now — a trampler assigns
        // everything to the player. CR 510.1c: without trample it assigns nothing.
        return StepAssignment {
            to_player: if has_trample { power } else { 0 },
            to_blockers: Vec::new(),
        };
    }

    let total_lethal: u32 = slots.iter().map(|&(_, owed)| owed).sum();

    // CR 702.19b: excess reaches the player only once EVERY blocker has been
    // assigned lethal damage.
    if has_trample && power > total_lethal {
        return StepAssignment {
            to_player: power - total_lethal,
            to_blockers: slots.to_vec(),
        };
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
    let mut budget = power;
    let to_blockers = slots
        .iter()
        .map(|&(id, owed)| {
            let amount = budget.min(owed);
            budget -= amount;
            (id, amount)
        })
        .collect();
    StepAssignment {
        to_player: 0,
        to_blockers,
    }
}

/// CR 120.4a + CR 120.10: the excess-damage reading — how much of a hit on this creature is
/// not excess.
///
/// Without deathtouch it is lethal damage less damage already marked, with no other combat
/// damage in flight. With a deathtouch source it is 1: "Any amount of damage greater than 1
/// is excess damage if the source dealing that damage to a creature has deathtouch" (CR
/// 120.4a). Public so consumers estimating how much a creature absorbs read the same number
/// the resolver uses rather than re-deriving it from raw toughness.
///
/// This is not the combat assignment minimum. Assignment reads
/// `lethal_damage::combat_lethal_damage_remaining` (CR 702.19b), which also counts damage
/// other attackers committed earlier in the step and never asks a deathtouch attacker for
/// more than the ordinary remainder. It owes 0 to a blocker that already has lethal damage,
/// or whose lethal damage is 0, where this reading is 1.
pub fn lethal_damage_needed(
    state: &GameState,
    object_id: ObjectId,
    source_has_deathtouch: bool,
) -> u32 {
    if source_has_deathtouch {
        // CR 120.4a: from a deathtouch source, any amount greater than 1 is excess.
        return 1;
    }
    state
        .objects
        .get(&object_id)
        .map(|obj| {
            // CR 120.6: lethal damage counts the damage already marked.
            lethal_damage::lethal_damage_threshold(state, obj).saturating_sub(obj.damage_marked)
        })
        .unwrap_or(1)
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
///   event but aggregates continuous all-damage and one-shot prevention riders.
///   `ShieldKind::PreventionOneShot` (Awe Strike) emits real source/recipient
///   `DamagePrevented` events per portion. `Prevention { All }` with
///   `consume_on_apply` (Impulsive Maneuvers' lose branch) uses the continuous-All
///   path and emits its aggregate event at the rider boundary. This pass can
///   park on a CR 616.1 ordering choice or a prompt raised by a replacement's
///   substitute (CR 616.1g); see [`CombatDamageBatch::Paused`].
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
    /// CR 616.1 + CR 616.1g: the batch parked on a choice made while one of its
    /// events is applied, and the unfinished rest is in
    /// `state.pending_combat_lifelink`. Five park points:
    /// * Phase B (`PendingCombatLifelink::replacement` is `Some`): a damage
    ///   event met two or more co-applicable replacements and the affected
    ///   player orders them (`state.pending_replacement` holds that event), or
    ///   a replacement's substitute raised a prompt of its own (counters, mill).
    ///   No damage has been dealt yet.
    /// * Phase C (`phase_c_remnant` is `Some`): a leg's life loss paused (Horde
    ///   damage->mill redirect, loss replacements); the unapplied legs ride the
    ///   remnant. A gain staged ahead of a leg can also park there, on its own
    ///   ordering choice or its substitute's prompt.
    /// * The lifelink tail: a lifelink life-gain event met two or more
    ///   co-applicable replacement effects. All damage is already dealt;
    ///   `state.pending_replacement` holds the paused source's own `LifeGain`.
    /// * The lifelink tail's substitute (CR 614.6 + CR 616.2): a gain's
    ///   substitute asked a choice of its own (Nefarious Lich's draws meeting
    ///   Abundance). The gain itself is settled; `remaining` holds the later
    ///   gains.
    /// * Phase D (CR 615.5 + CR 608.2d): a prevention rider asked its player a
    ///   choice (Channel Harm's "may"). `prevention_tally` holds the shields not
    ///   yet published.
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

/// CR 510.2: Phase C accumulators — the lifelink sums and combat bookkeeping
/// folded per applied leg, and the lifelink gains staged ahead of a leg (CR
/// 101.4: settled with the rest of the batch's gains, never here). Owned so a
/// leg pause can park them in the Phase C remnant and the resume can continue
/// exactly where the batch parked.
struct PhaseCAccumulators {
    lifelink_by_source: Vec<(ObjectId, PlayerId, u32)>,
    combat_damage_to_players: Vec<PerPlayerCombatDamage>,
    source_incarnations: Vec<ObjectIncarnationRef>,
    staged_gains: Vec<PendingLifelinkGain>,
}

/// CR 510.2 + CR 616.1: outcome of running Phase C legs.
enum PhaseCRunOutcome {
    Complete,
    /// Leg `index` paused on a CR 616.1 replacement choice over its results:
    /// its life loss or its poison counters (infect/toxic). Its remaining
    /// results ride a combat stash when any are owed (a loss pause always; an
    /// infect pause when toxic is owed); a stashless pause is a toxic or an
    /// owed-nothing infect pause whose fact was already recorded. The leg's
    /// lifelink share and bookkeeping are already folded into the
    /// accumulators; legs after `index` never applied.
    PausedOnReplacement {
        index: usize,
    },
    /// CR 616.1 + CR 101.4: a lifelink gain staged ahead of `legs[index]`
    /// (an earlier-APNAP player's choice-capable gain before a leg whose
    /// result can open a later player's choice, or a gain of the player the
    /// leg damages) paused on its own replacement choice or its substitute
    /// before that leg applied. The tail to park is `legs[index..]`; the
    /// paused gain's legs already have their lifelink markers cleared.
    PausedOnGain {
        index: usize,
        pause: GainStagePause,
    },
    /// CR 120.3d + CR 616.1: leg `index` paused on its `-1/-1` counter
    /// replacements (a combat-flagged counter stash is parked). Same folding
    /// contract as `PausedOnReplacement`: the leg's lifelink share and bookkeeping
    /// are already folded into the accumulators; legs after `index` never
    /// applied. The resume runs the core ONLY (no lifelink — it drains via
    /// the folded tail).
    PausedOnCounter {
        index: usize,
    },
}

/// CR 510.2 + CR 616.1: outcome of completing a parked Phase C tail.
enum RemnantOutcome {
    Complete,
    /// `on_gain` marks a pause awaiting the staged gain's own replacement answer.
    Repaused {
        remnant: PendingCombatPhaseCRemnant,
        on_gain: bool,
    },
}

/// CR 119.3 + CR 702.15b + CR 510.2 + CR 704.6c: fold one leg's
/// post-replacement amount into the lifelink sums and the combat bookkeeping.
/// Amounts are order-free, so a paused leg folds EAGERLY at park time (from
/// its stash) and the resume never re-folds it — only its `DamageDealt`
/// event + CR 120.1 record wait for the loss to settle (sync order).
fn fold_combat_leg_amount(
    state: &mut GameState,
    leg: &PendingCombatLeg,
    actual_amount: u32,
    accums: &mut PhaseCAccumulators,
) {
    // CR 119.3 + CR 702.15b: accumulate this source's lifelink over the batch
    // (damage to any recipient — creature, planeswalker, or player — feeds the
    // same life gain). Keyed by source so two different lifelink sources stay
    // separate events while one source hitting many targets is summed.
    if let Some(controller) = leg.lifelink_controller {
        if actual_amount > 0 {
            let source_id = leg.ctx.source_id;
            if let Some((_, _, total)) = accums
                .lifelink_by_source
                .iter_mut()
                .find(|(id, _, _)| *id == source_id)
            {
                *total += actual_amount;
            } else {
                accums
                    .lifelink_by_source
                    .push((source_id, controller, actual_amount));
            }
        }
    }

    // Combat-only bookkeeping (not part of the shared damage pipeline).
    // CR 614.6: a replaced event never happens, so the bookkeeping follows the
    // SURVIVING event's recipient, not the assigned one — a hit redirected
    // away from the player credits that player nothing. CR 120.8: a source
    // that would deal 0 damage deals no damage at all, so a zero delivered
    // amount (fully replaced or prevented) records no hit either; CR 903.10a
    // and the Ring level-four trigger count damage actually dealt.
    let delivered_player = match leg.survivor.as_ref() {
        Some(ProposedEvent::Damage {
            target: TargetRef::Player(player_id),
            ..
        }) if actual_amount > 0 => Some(*player_id),
        _ => None,
    };
    if let Some(player_id) = &delivered_player {
        let source_id = leg.ctx.source_id;
        // CR 510.2: Track per-source amounts for this step. Each source
        // appears at most once per player per step; dedup guards any edge
        // where the same source is re-applied (e.g. split-damage riders).
        if let Some(incarnation) = leg.ctx.source_incarnation {
            let source =
                crate::types::identifiers::ObjectIncarnationRef::of(source_id, incarnation);
            if !accums.source_incarnations.contains(&source) {
                accums.source_incarnations.push(source);
            }
        }
        if let Some((_, sources, total)) = accums
            .combat_damage_to_players
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
            accums.combat_damage_to_players.push((
                *player_id,
                vec![(source_id, actual_amount)],
                actual_amount,
            ));
        }

        // CR 704.6c: Track commander combat damage for the 21-damage loss condition.
        if leg.source_is_commander && actual_amount > 0 {
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

/// CR 510.2 + CR 616.1 + CR 101.4: order the simultaneous Phase C legs so the
/// choices their results need are made in APNAP order. CR 616.1 gives a
/// result's replacement choice to the affected player, and players choosing at
/// the same time choose in APNAP order (CR 101.4). Phase B swallows choices,
/// so only Phase C prompts, and it applies each leg's SURVIVOR event: a leg
/// whose survivor deals damage to a player can pause on that player's choice
/// (keyed by `apnap_team_rank` of that player — not the attacker's
/// controller, the actor, nor the assignment's target, which a redirection
/// shield can change). Legs whose survivor is `None` (fully prevented, applied
/// as 0) or damages an object keep their slots; the player legs are
/// stable-sorted among their own slots, so equal ranks (one player, or one
/// shared-turn team) keep declaration order and 2-player / creature-only
/// batches are unchanged.
fn order_phase_c_legs_by_apnap(state: &GameState, legs: &mut [PendingCombatLeg]) {
    reorder_ranked_slots(legs, |leg| survivor_player_rank(state, leg));
}

/// CR 616.1 + CR 101.4: the APNAP rank of the player whose CR 616.1 choice a
/// leg's results belong to: the player its SURVIVOR damages (a redirection
/// changes the survivor, so a redirected leg is keyed by the receiver).
/// `None` for a `None` survivor or one that damages an object.
fn survivor_player_rank(state: &GameState, leg: &PendingCombatLeg) -> Option<usize> {
    match &leg.survivor {
        Some(ProposedEvent::Damage {
            target: TargetRef::Player(player),
            ..
        }) => Some(crate::game::topology::apnap_team_rank(state, *player)),
        _ => None,
    }
}

/// Stable-sort the ranked items among their own slots: unranked items keep
/// their slots and equal ranks keep their relative order. Shared by the
/// Phase C legs and the lifelink gain queue.
fn reorder_ranked_slots<T: Clone>(items: &mut [T], rank: impl Fn(&T) -> Option<usize>) {
    let slots: Vec<(usize, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(slot, item)| rank(item).map(|r| (slot, r)))
        .collect();
    if slots.len() < 2 {
        return;
    }
    let mut ordered = slots.clone();
    ordered.sort_by_key(|&(_, r)| r);
    let ordered_items: Vec<T> = ordered
        .iter()
        .map(|&(slot, _)| items[slot].clone())
        .collect();
    for (&(slot, _), item) in slots.iter().zip(ordered_items) {
        items[slot] = item;
    }
}

/// CR 616.1 + CR 101.4 + CR 702.15b: the APNAP rank of a lifelink gain's
/// first prompt. A gain's replacement order belongs to the gaining player.
/// A substitute can address another player when the gain itself has no prompt.
/// A gain with no rank keeps its slot and is staged ahead of a leg only when
/// its owner is the player that leg damages.
fn lifelink_gain_rank(state: &GameState, controller: PlayerId, amount: u32) -> Option<usize> {
    use crate::game::effects::life;
    use crate::game::topology::apnap_team_rank;
    let event = life::proposed_life_gain(state, controller, amount)?;
    let mut first = None;
    replacement::visit_proposed_event_choosers(state, &event, &mut |player| {
        first = Some(apnap_team_rank(state, player));
    });
    first.or_else(|| {
        life::life_gain_may_prompt(state, controller, amount)
            .then(|| apnap_team_rank(state, controller))
    })
}

/// CR 120.4b: what a not-yet-applied leg will deal: its survivor's amount, or
/// `0` for a `None`/non-damage survivor and for a phased-out target player
/// (mirrors `apply_damage_after_replacement`'s `Applied(0)` early return).
/// Used only to total a source's lifelink before its legs apply; the fold
/// keeps using the applied amount.
fn planned_leg_amount(state: &GameState, leg: &PendingCombatLeg) -> u32 {
    match &leg.survivor {
        Some(ProposedEvent::Damage { target, amount, .. }) => {
            let phased_out_player = matches!(target, TargetRef::Player(player)
                if state
                    .players
                    .iter()
                    .find(|p| p.id == *player)
                    .is_some_and(|p| p.is_phased_out()));
            if phased_out_player {
                0
            } else {
                *amount
            }
        }
        _ => 0,
    }
}

/// CR 616.1 + CR 101.4 + CR 510.2: the leg-side gate of staging an
/// earlier-APNAP player's gain ahead of a leg. That staging exists only so
/// the choice is asked before a LATER player's result choice; a leg whose
/// result cannot open a choice needs no reordering, so an ordinary lifelink
/// attack keeps its event order.
fn combat_leg_may_prompt(state: &GameState, leg: &PendingCombatLeg) -> bool {
    leg.survivor.as_ref().is_some_and(|survivor| {
        crate::game::effects::deal_damage::combat_leg_may_prompt(
            state,
            &DamageContext::from(leg.ctx),
            survivor,
        )
    })
}

/// Outcome of staging one source's combat lifelink gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GainStageOutcome {
    /// The gain's replacement choices are made: its final post-replacement
    /// amount is owed, to be settled with the batch's other gains.
    Staged(PendingLifelinkGain),
    /// Nothing is owed: the gain never reached the pipeline, was fully
    /// replaced, or settled inline with its substitute (Lich class).
    Done,
    Paused(GainStagePause),
    GameOver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GainStagePause {
    ReplacementChoice,
    SubstitutionContinuation,
}

/// CR 616.1 + CR 702.15e + CR 101.4: stage one source's combat lifelink gain
/// as its own life-gain event. Hold final gains for simultaneous settlement
/// while any cross-event substitute runs through its own continuation. Both
/// `drain_combat_lifelink` and the staging ahead of a Phase C leg use this authority.
///
/// On a CR 616.1 choice NOTHING was staged; `state.pending_replacement` is
/// left populated ON PURPOSE: it is the resume's authority (the answered final
/// event is recorded by `take_staged_answer`), and the caller never re-queues
/// this source (that would gain the life twice).
fn stage_combat_lifelink_gain(
    state: &mut GameState,
    gain: PendingLifelinkGain,
    batch_events: &mut Vec<GameEvent>,
) -> GainStageOutcome {
    let outcome = crate::game::effects::life::stage_life_gain(
        state,
        gain.controller,
        gain.amount,
        batch_events,
    );
    // CR 104.1: a substitute can end the game (Laboratory Maniac's win).
    if state.game_end.is_some() {
        crate::game::elimination::ensure_game_over_if_terminal(state, batch_events);
        return GainStageOutcome::GameOver;
    }
    match outcome {
        Ok(StagedLifeGain::Final(ProposedEvent::LifeGain {
            player_id, amount, ..
        })) => GainStageOutcome::Staged(PendingLifelinkGain {
            controller: player_id,
            amount,
        }),
        Ok(StagedLifeGain::Final(other)) => {
            debug_assert!(
                false,
                "a staged life gain must stay a LifeGain, got {other:?}"
            );
            GainStageOutcome::Done
        }
        Ok(StagedLifeGain::Settled(_)) => GainStageOutcome::Done,
        Err(ReplacementDeferred::ReplacementChoice) => {
            GainStageOutcome::Paused(GainStagePause::ReplacementChoice)
        }
        // CR 614.6 + CR 614.1: a cross-event substitute (Lich class) already
        // ran its replacing action. On Execute the gain was APPLIED before
        // `drain_substitution_continuation` ran, and on Prevented nothing was
        // owed — so this source's gain is not re-queued.
        //
        // CR 614.6 + CR 616.2: the substitute's own action stopped at a choice
        // (Nefarious Lich's draws meeting Abundance's "may"). Park the rest of
        // the batch beneath that prompt and let the player's answer resume it.
        Err(ReplacementDeferred::SubstitutionContinuation { .. }) => {
            GainStageOutcome::Paused(GainStagePause::SubstitutionContinuation)
        }
    }
}

/// CR 616.1 + CR 101.4 + CR 101.4c + CR 510.2 + CR 702.15b: before applying
/// `legs[index]`, whose survivor damages the player ranked `rank`, stage the
/// lifelink gains whose choices come first, in rank order:
/// - every choice-capable gain owned by a STRICTLY earlier-APNAP player, and
///   only when the leg's result can open a choice (`combat_leg_may_prompt`):
///   the damage and its lifelink gain are simultaneous results of the batch
///   (CR 510.2, CR 120.3f), so their choices are made in APNAP order;
/// - Gains of an equal-rank player (the damaged player or a shared-turn
///   teammate, CR 805.6) whose chooser has the same or an earlier rank, plus
///   choice-free gains. Staging the gain before the leg writes the board
///   makes both read the pre-batch board. A
///   gain's substitute runs during staging (CR 614.6), so it can end the game
///   before the leg is applied (CR 104.1).
///
/// Final gains settle with the batch's other gains (`drain_combat_lifelink`).
/// Totals per source are the gains folded so far plus the planned amounts of
/// `legs[index..]` (the per-source sum the fold produces, in first-seen
/// order: aggregation is unchanged). A staged
/// source is removed from the sums and its remaining legs'
/// `lifelink_controller` markers are cleared, so the later fold cannot gain it
/// again. A pause carries the kind of answer the batch is awaiting.
fn stage_earlier_apnap_lifelink(
    state: &mut GameState,
    legs: &mut [PendingCombatLeg],
    index: usize,
    rank: usize,
    accums: &mut PhaseCAccumulators,
    events: &mut Vec<GameEvent>,
) -> Option<GainStagePause> {
    let leg_may_prompt = combat_leg_may_prompt(state, &legs[index]);
    let mut totals = accums.lifelink_by_source.clone();
    for leg in &legs[index..] {
        let Some(controller) = leg.lifelink_controller else {
            continue;
        };
        let amount = planned_leg_amount(state, leg);
        if amount == 0 {
            continue;
        }
        match totals
            .iter_mut()
            .find(|(id, _, _)| *id == leg.ctx.source_id)
        {
            Some((_, _, total)) => *total += amount,
            None => totals.push((leg.ctx.source_id, controller, amount)),
        }
    }
    let mut due: Vec<(usize, ObjectId, PlayerId, u32)> = totals
        .into_iter()
        .filter_map(|(source_id, controller, amount)| {
            let gain_rank = crate::game::topology::apnap_team_rank(state, controller);
            let chooser_rank = lifelink_gain_rank(state, controller, amount);
            let earlier = leg_may_prompt && chooser_rank.is_some_and(|chooser| chooser < rank);
            let scheduled_rank = chooser_rank.unwrap_or(gain_rank);
            let same_team = gain_rank == rank && scheduled_rank <= rank;
            (earlier || same_team).then_some((scheduled_rank, source_id, controller, amount))
        })
        .collect();
    due.sort_by_key(|&(gain_rank, ..)| gain_rank);
    for (_, source_id, controller, amount) in due {
        accums
            .lifelink_by_source
            .retain(|(id, _, _)| *id != source_id);
        for leg in legs[index..].iter_mut() {
            if leg.ctx.source_id == source_id {
                leg.lifelink_controller = None;
            }
        }
        match stage_combat_lifelink_gain(state, PendingLifelinkGain { controller, amount }, events)
        {
            GainStageOutcome::Staged(staged) => accums.staged_gains.push(staged),
            GainStageOutcome::Done => {}
            GainStageOutcome::Paused(pause) => return Some(pause),
            GainStageOutcome::GameOver => break,
        }
    }
    None
}

/// CR 616.1 + CR 101.4 + CR 702.15e: the tail gain queue, one gain per
/// source. The gains whose own pipeline can open a choice are stable-sorted
/// among their own slots by the gaining player's APNAP rank; other gains keep
/// their slots, so an ordinary multi-controller lifelink batch emits exactly
/// as before.
fn planned_gain_queue(
    state: &GameState,
    lifelink_by_source: Vec<(ObjectId, PlayerId, u32)>,
) -> std::collections::VecDeque<PendingLifelinkGain> {
    let mut gains: Vec<PendingLifelinkGain> = lifelink_by_source
        .into_iter()
        .map(|(_source_id, controller, amount)| PendingLifelinkGain { controller, amount })
        .collect();
    reorder_ranked_slots(&mut gains, |gain| {
        lifelink_gain_rank(state, gain.controller, gain.amount)
    });
    gains.into()
}

/// CR 510.2 + CR 120.3 + CR 616.1: apply Phase C legs in batch order —
/// shared by the fresh batch and the remnant resume. A leg whose life loss
/// or infect result pauses with results still owed (a combat-flagged stash
/// that was absent before this leg) or whose poison counters pause with
/// nothing owed (no stash) folds eagerly and parks the tail so the prompt
/// surfaces; a leg whose `-1/-1` counter placement pauses (a combat-flagged
/// counter stash that was absent before this leg) folds eagerly and parks the
/// same way; `None` survivors apply as 0 with bookkeeping, exactly as the
/// never-paused path would. Before a leg that damages a player, the lifelink
/// gains whose choices come first are staged (`stage_earlier_apnap_lifelink`),
/// which can clear `lifelink_controller` markers on `legs[index..]`.
fn run_phase_c_legs(
    state: &mut GameState,
    legs: &mut [PendingCombatLeg],
    accums: &mut PhaseCAccumulators,
    events: &mut Vec<GameEvent>,
) -> PhaseCRunOutcome {
    for index in 0..legs.len() {
        // CR 616.1 + CR 101.4: this leg's results belong to the damaged
        // player; an earlier-APNAP player's choice-capable gain (when the leg
        // can prompt) and that player's own gains are staged first.
        if let Some(rank) = survivor_player_rank(state, &legs[index]) {
            if let Some(pause) =
                stage_earlier_apnap_lifelink(state, legs, index, rank, accums, events)
            {
                return PhaseCRunOutcome::PausedOnGain { index, pause };
            }
            if state.game_end.is_some() {
                break;
            }
        }
        let leg = &legs[index];
        let actual_amount = match leg.survivor.clone() {
            // CR 120.3 + CR 120.4b: Apply the post-replacement event WITHOUT
            // re-running the replacement pipeline.
            Some(survivor_event) => {
                // Snapshot BEFORE applying: a combat-flagged stash that
                // appears here identifies THIS leg's pause with results
                // parked (a loss pause, or an infect pause that owes toxic).
                // Both fold `stashed.actual_amount`, which equals the
                // survivor's `dealt_amount` (players have no excess).
                let had_loss_stash = state.pending_damage_post_loss.is_some();
                let had_counter_stash = state.pending_damage_post_counter.is_some();
                // CR 120.4b: the post-replacement amount this leg deals, read
                // before the apply consumes the event (a counter pause leaves
                // no stash to read it from).
                let dealt_amount = match &survivor_event {
                    ProposedEvent::Damage { amount, .. } => *amount,
                    _ => 0,
                };
                let ctx = DamageContext::from(leg.ctx);
                match apply_damage_after_replacement(state, &ctx, survivor_event, true, events) {
                    DamageResult::Applied(amt) => amt,
                    DamageResult::NeedsChoice => {
                        let mut loss_pause_amount = None;
                        if !had_loss_stash {
                            if let Some(stashed) = state.pending_damage_post_loss.as_ref() {
                                if stashed.is_combat {
                                    loss_pause_amount = Some(stashed.actual_amount);
                                }
                            }
                        }
                        if let Some(amt) = loss_pause_amount {
                            fold_combat_leg_amount(state, leg, amt, accums);
                            return PhaseCRunOutcome::PausedOnReplacement { index };
                        }
                        // CR 120.3d + CR 616.1: a `-1/-1` counter placement paused —
                        // same folding contract as the loss pause: fold eagerly, park
                        // the tail; the resume runs the core ONLY (lifelink arrives
                        // via the folded tail).
                        if !had_counter_stash {
                            if let Some(stashed) = state.pending_damage_post_counter.as_ref() {
                                if stashed.is_combat {
                                    let amt = stashed.actual_amount;
                                    fold_combat_leg_amount(state, leg, amt, accums);
                                    return PhaseCRunOutcome::PausedOnCounter { index };
                                }
                            }
                        }
                        // CR 120.4c + CR 616.1: no fresh stash means the leg
                        // paused on its poison-counter result with nothing
                        // owed behind it (a toxic pause, or an infect pause
                        // without toxic).
                        // That identification holds because the combat ctx has
                        // `has_lifelink` forced off (lifelink folds per source
                        // in `apply_combat_damage`) and no `excess_recipient`,
                        // so every other combat `NeedsChoice` is a stashing
                        // one (the loss pause, or an infect pause owing
                        // toxic). The damage fact was recorded at the
                        // pause; fold the dealt amount like an unpaused leg and
                        // park the tail so the ordering prompt surfaces and the
                        // counter answer resumes the batch. A future combat
                        // `NeedsChoice` source must be classified here, not
                        // folded as a counter pause.
                        debug_assert!(
                            !ctx.has_lifelink && ctx.excess_recipient.is_none(),
                            "combat Phase C leg paused without a stash outside the \
                             poison-counter path"
                        );
                        debug_assert!(
                            matches!(
                                state.pending_replacement.as_ref().map(|p| &p.proposed),
                                Some(ProposedEvent::AddCounter { .. })
                            ),
                            "stashless combat leg pause must be a counter-addition choice"
                        );
                        fold_combat_leg_amount(state, leg, dealt_amount, accums);
                        return PhaseCRunOutcome::PausedOnReplacement { index };
                    }
                }
            }
            // Fully prevented or skipped — no damage applied.
            None => 0,
        };
        fold_combat_leg_amount(state, leg, actual_amount, accums);
    }
    PhaseCRunOutcome::Complete
}

/// CR 510.2: the per-player aggregate, built in exactly the order it has
/// always been emitted and carried to the drain so a pause cannot reorder it
/// ahead of a resumed gain.
fn build_combat_damage_to_players_aggregate(
    combat_damage_to_players: Vec<PerPlayerCombatDamage>,
    source_incarnations: &[ObjectIncarnationRef],
) -> Vec<GameEvent> {
    combat_damage_to_players
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
        .collect()
}

/// CR 510.2 + CR 616.1: complete a parked Phase C tail — settle the paused
/// leg's parked results (a loss stash's toxic poison and deferred `DamageDealt`
/// event + CR 120.1 record when still owed, or a counter stash's deferred
/// remainder), apply the remaining legs, then build the aggregate and the
/// per-source lifelink gains onto the record. Only called once the paused
/// result fully settles. A parked result that pauses again re-parks the
/// remnant UNCHANGED: its `remaining` legs have not run, and the paused leg's
/// amounts folded eagerly at park time.
fn complete_combat_phase_c_remnant(
    state: &mut GameState,
    remnant: PendingCombatPhaseCRemnant,
    record: &mut PendingCombatLifelink,
) -> RemnantOutcome {
    // The paused leg's lifelink share and bookkeeping folded eagerly at park
    // time; only its parked results were deferred (sync order: after the
    // paused result settles). A missing stash is expected when the paused leg
    // paused on a poison-counter result that owed nothing more (a toxic pause,
    // or an infect pause without toxic: the damage fact was recorded at the
    // pause). Otherwise it is unreachable, because the funnel skips combat
    // stashes. Continuing with the remaining legs (rather than panicking)
    // keeps a corrupt save resumable.
    if let Some(stashed) = state
        .pending_damage_post_loss
        .take_if(|stashed| stashed.is_combat)
    {
        if settle_parked_player_damage_results(state, *stashed, &mut record.batch_events)
            == DamagePostLossDrainOutcome::PausedForReplacement
        {
            return RemnantOutcome::Repaused {
                remnant,
                on_gain: false,
            };
        }
    }
    // CR 120.3d + CR 616.1: the same contract for a counter-paused leg — run
    // the core ONLY (F1 split: lifelink already folded at park time). A core
    // pause hands ownership to the redirect leg's own stash, exactly as the
    // loss path leaves a re-parked loss stash for the code above on re-entry.
    if let Some(stashed) = state
        .pending_damage_post_counter
        .take_if(|stashed| stashed.is_combat)
    {
        let ctx = DamageContext::from(stashed.ctx);
        // CR 120.10: the remnant passes the stashed excess, never a recompute.
        match wither_infect_post_counter_core(
            state,
            &ctx,
            &stashed.target,
            stashed.actual_amount,
            stashed.excess,
            true,
            &mut record.batch_events,
        ) {
            PostCounterFlow::Paused => {}
            PostCounterFlow::Continue { .. } => {}
        }
    }
    let PendingCombatPhaseCRemnant {
        mut remaining,
        lifelink_by_source,
        combat_damage_to_players,
        source_incarnations,
        staged_gains,
    } = remnant;
    let mut accums = PhaseCAccumulators {
        lifelink_by_source,
        combat_damage_to_players,
        source_incarnations,
        staged_gains,
    };
    match run_phase_c_legs(state, &mut remaining, &mut accums, &mut record.batch_events) {
        PhaseCRunOutcome::Complete => {
            record.damage_to_players = build_combat_damage_to_players_aggregate(
                accums.combat_damage_to_players,
                &accums.source_incarnations,
            );
            record.remaining = planned_gain_queue(state, accums.lifelink_by_source);
            // CR 101.4: gains staged ahead of a leg settle with the tail's.
            record.staged_gains.append(&mut accums.staged_gains);
            RemnantOutcome::Complete
        }
        PhaseCRunOutcome::PausedOnGain { index, pause } => RemnantOutcome::Repaused {
            remnant: PendingCombatPhaseCRemnant {
                remaining: remaining[index..].to_vec(),
                lifelink_by_source: accums.lifelink_by_source,
                combat_damage_to_players: accums.combat_damage_to_players,
                source_incarnations: accums.source_incarnations,
                staged_gains: accums.staged_gains,
            },
            on_gain: pause == GainStagePause::ReplacementChoice,
        },
        PhaseCRunOutcome::PausedOnReplacement { index } => RemnantOutcome::Repaused {
            remnant: PendingCombatPhaseCRemnant {
                remaining: remaining[index + 1..].to_vec(),
                lifelink_by_source: accums.lifelink_by_source,
                combat_damage_to_players: accums.combat_damage_to_players,
                source_incarnations: accums.source_incarnations,
                staged_gains: accums.staged_gains,
            },
            on_gain: false,
        },
        // CR 120.3d + CR 616.1: same re-park shape as the loss pause — the
        // paused leg rides its combat-flagged counter stash.
        PhaseCRunOutcome::PausedOnCounter { index } => RemnantOutcome::Repaused {
            remnant: PendingCombatPhaseCRemnant {
                remaining: remaining[index + 1..].to_vec(),
                lifelink_by_source: accums.lifelink_by_source,
                combat_damage_to_players: accums.combat_damage_to_players,
                source_incarnations: accums.source_incarnations,
                staged_gains: accums.staged_gains,
            },
            on_gain: false,
        },
    }
}

/// CR 101.4: a player's position in APNAP order (active player first, then
/// each other living player in turn order); players outside it sort last. The
/// order is computed once, so a sort key built from the returned closure does
/// not rebuild it per comparison.
fn apnap_ranker(state: &GameState) -> impl Fn(PlayerId) -> usize {
    let order = super::players::apnap_order(state);
    move |player| {
        order
            .iter()
            .position(|seat| *seat == player)
            .unwrap_or(usize::MAX)
    }
}

/// CR 616.1 + CR 101.4 + CR 510.2: combat damage is dealt simultaneously, so
/// the order in which attackers were declared and their damage assigned
/// carries no rules weight; when several players must each choose among
/// replacement effects for the batch, "choices are made in APNAP order". The
/// pass proposes events one at a time and re-evaluates each against the
/// current state after every answer, so ordering the batch by the APNAP rank
/// of each event's affected player (a damaged player, or the controller of a
/// damaged permanent — the chooser CR 616.1 names) makes the prompts arrive
/// active player first, then each nonactive player in turn order. The sort is
/// stable: one seat's events keep their assignment order, and `entries` stays
/// aligned 1:1 with the events.
/// CR 616.1: the seat that chooses among replacements for a proposed damage
/// event as first proposed — a damaged player, or the controller of a damaged
/// permanent. `None` for a non-damage event or a recipient that left the game.
pub(crate) fn combat_damage_affected_seat(
    state: &GameState,
    event: &ProposedEvent,
) -> Option<PlayerId> {
    let ProposedEvent::Damage { target, .. } = event else {
        return None;
    };
    match target {
        TargetRef::Player(player) => Some(*player),
        TargetRef::Object(id) => state.objects.get(id).map(|obj| obj.controller),
    }
}

fn order_batch_by_affected_seat(
    state: &GameState,
    entries: &mut Vec<PendingCombatDamageEntry>,
    proposed_events: &mut VecDeque<ProposedEvent>,
) {
    let rank_of_seat = apnap_ranker(state);
    let rank = |event: &ProposedEvent| -> usize {
        combat_damage_affected_seat(state, event).map_or(usize::MAX, &rank_of_seat)
    };
    let mut paired: Vec<(PendingCombatDamageEntry, ProposedEvent)> = std::mem::take(entries)
        .into_iter()
        .zip(std::mem::take(proposed_events))
        .collect();
    paired.sort_by_key(|(_, event)| rank(event));
    for (entry, event) in paired {
        entries.push(entry);
        proposed_events.push_back(event);
    }
}

pub(crate) fn apply_combat_damage(
    state: &mut GameState,
    assignments: &[(ObjectId, DamageAssignment)],
    sub_step: CombatDamageSubStep,
) -> CombatDamageBatch {
    let events = Vec::new();

    // --- Phase A: Collect proposed damage events (CR 510.2) ---
    // Gated assignments (0-damage, protection, prohibition) contribute nothing
    // and are dropped here; the gate already emitted any required DamagePrevented.
    let mut entries: Vec<PendingCombatDamageEntry> = Vec::with_capacity(assignments.len());
    let mut proposed_events: VecDeque<ProposedEvent> = VecDeque::with_capacity(assignments.len());
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

        if let DamageGateOutcome::Proposed(proposed) =
            pre_replacement_damage_gate(state, &ctx, &target_ref, assignment.amount, true)
        {
            // CR 510.2: the entry carries the damage context, commander flag and
            // recipient alongside the source's `ProposedEvent`, so Phase C can
            // apply the post-replacement survivor and run combat-only bookkeeping
            // — snapshotted, because a CR 616.1 pause can park the batch.
            entries.push(PendingCombatDamageEntry {
                context: DamageContextSnapshot::from(&ctx),
                source_is_commander,
                target: assignment.target.clone(),
                lifelink_controller,
            });
            proposed_events.push_back(proposed);
        }
    }

    // CR 616.1 + CR 101.4: the batch's replacement prompts follow APNAP order
    // of the affected seats, not assignment order.
    order_batch_by_affected_seat(state, &mut entries, &mut proposed_events);

    // --- Phase B: Batch replacement (CR 510.2 + CR 615.7) ---
    let record = PendingCombatLifelink {
        remaining: VecDeque::new(),
        staged_gains: Vec::new(),
        awaiting_staged_gain: false,
        batch_events: events,
        damage_to_players: Vec::new(),
        prevention_tally: Vec::new(),
        sub_step,
        replacement: None,
        phase_c_remnant: None,
    };
    // CR 608.2f + CR 700.1: the pass reads the printed guards once, as the batch opens.
    let pass = replacement::CombatDamageReplacementPass::new(state, proposed_events);
    run_combat_damage_replacements(state, record, entries, pass, None)
}

/// CR 510.2 + CR 615.7 + CR 616.1: Phase B of one simultaneous combat-damage
/// batch — the replacement pass over its damage events — then Phase C and the
/// completion tail.
///
/// The pass parks on a CR 616.1 ordering choice or on a prompt its substitute
/// raised (CR 616.1g); the whole batch is then parked in
/// `state.pending_combat_lifelink` with `replacement` set, exactly where a
/// lifelink pause parks it, and `resume_pending_combat_lifelink` re-enters here
/// with `resume`. Nothing is dealt before the pass completes, so the resumed
/// batch applies each event once.
fn run_combat_damage_replacements(
    state: &mut GameState,
    mut record: PendingCombatLifelink,
    entries: Vec<PendingCombatDamageEntry>,
    mut pass: replacement::CombatDamageReplacementPass,
    resume: Option<replacement::CombatDamageReplacementResume>,
) -> CombatDamageBatch {
    match replacement::replace_combat_damage_batch(
        state,
        &mut record.batch_events,
        &mut pass,
        resume,
    ) {
        replacement::CombatDamageReplacementOutcome::Complete => {
            // CR 104.1: a game that ended inside the pass ends the batch: nothing is dealt,
            // gained or prevented after it.
            if state.game_end.is_some() {
                return CombatDamageBatch::Complete(record.batch_events);
            }
            apply_combat_damage_survivors(state, record, entries, pass)
        }
        replacement::CombatDamageReplacementOutcome::Paused { waiting_for } => {
            // CR 616.1: nothing of this batch has been dealt; park all of it.
            // The tally is carried as a `Vec` (an enum key is not a JSON map key)
            // and reinstalled on resume. CR 615.8: the one-shot application
            // ledger is parked with it, so a "next time" shield that already
            // applied to an earlier portion still covers the source's portions
            // proposed after the pause, and the printed guards' reading the pass
            // opened with is parked too, so every later portion meets that one
            // reading (CR 608.2f). The tally is sorted so a parked state
            // serializes identically however the map iterated.
            record.prevention_tally = sorted_prevention_entries(pass.tally);
            record.replacement = Some(PendingCombatDamageReplacement {
                entries,
                resolved: pass.resolved,
                one_shot_applications: pass.one_shot_applications,
                settled_guards: pass.settled_guards,
            });
            state.waiting_for = (*waiting_for).clone();
            let events = record.batch_events.clone();
            state.pending_combat_lifelink = Some(Box::new(record));
            CombatDamageBatch::Paused {
                events,
                waiting_for,
            }
        }
    }
}

/// CR 616.1 + CR 616.1g: resume a batch parked in its replacement pass. An
/// ordering choice is answered here, with `choice`; a substitute's prompt has
/// already been answered by its own dispatcher, and the pass picks up the
/// parked entry's settled survivor.
fn resume_combat_damage_replacements(
    state: &mut GameState,
    mut record: PendingCombatLifelink,
    choice: Option<usize>,
) -> CombatDamageBatch {
    let Some(parked) = record.replacement.take() else {
        return drain_combat_lifelink(state, record);
    };
    let resume = replacement::CombatDamageReplacementResume { choice };
    let pass = replacement::CombatDamageReplacementPass {
        resolved: parked.resolved,
        tally: tally_map_from_record_entries(&std::mem::take(&mut record.prevention_tally)),
        one_shot_applications: parked.one_shot_applications,
        settled_guards: parked.settled_guards,
    };
    run_combat_damage_replacements(state, record, parked.entries, pass, Some(resume))
}

/// True when the parked combat-damage batch is waiting on the CR 616.1 ordering
/// choice for one of its own damage events — the `ChooseReplacement` that
/// answers it belongs to the batch (`resume_pending_combat_lifelink` with a
/// choice), not to the generic replacement-choice dispatcher, which would deal
/// the damage outside the batch.
pub(crate) fn parked_combat_damage_awaits_replacement_choice(state: &GameState) -> bool {
    state
        .pending_combat_lifelink
        .as_ref()
        .is_some_and(|record| {
            record.replacement.as_ref().is_some_and(|parked| {
                parked.resolved.iter().any(|leg| {
                    matches!(
                        leg,
                        PendingCombatLegProgress::AwaitingChoice { parked: None, .. }
                    )
                })
            })
        })
}

/// CR 120.3 + CR 510.2: Phase C of a simultaneous combat-damage batch — apply
/// each surviving post-replacement event and run the combat bookkeeping, then
/// hand the tail (lifelink gains, per-player aggregate, Phase D riders) to
/// [`drain_combat_lifelink`]. Runs only once the replacement pass is complete.
fn apply_combat_damage_survivors(
    state: &mut GameState,
    mut record: PendingCombatLifelink,
    entries: Vec<PendingCombatDamageEntry>,
    pass: replacement::CombatDamageReplacementPass,
) -> CombatDamageBatch {
    // Deterministic order: the tally arrives as a `HashMap`, but it is stored in
    // `PendingCombatLifelink` (which is `Serialize`/`Deserialize`, so a parked
    // batch must round-trip byte-identically) and Phase D fires one CR 615.5
    // rider per entry IN THIS ORDER. A `HashMap`'s arbitrary iteration order
    // would let the same batch serialize — and emit its rider events —
    // differently across saves and AI clones. The ids are not `Ord`, so sort on
    // their inner integers, exactly as `blocker_ids` is sorted above. Sorted
    // here (rather than after Phase C) so a leg pause parks the same order the
    // drain would have seen; Phase C never touches the tally.
    let prevention_tally = sorted_prevention_entries(pass.tally);

    // --- Phase C: Apply survivors + combat bookkeeping (CR 120.3 + CR 510.2) ---
    // Owned legs so a leg pause parks the unapplied tail in the remnant and the
    // resume shares the same runner.
    let survivors = replacement::settled_survivors(pass.resolved);
    debug_assert_eq!(
        entries.len(),
        survivors.len(),
        "batch survivor count must align with collected entries"
    );
    let mut legs: Vec<PendingCombatLeg> = entries
        .into_iter()
        .zip(survivors)
        .map(|(entry, survivor)| PendingCombatLeg {
            ctx: entry.context,
            source_is_commander: entry.source_is_commander,
            target: match entry.target {
                DamageTarget::Object(id) => TargetRef::Object(id),
                DamageTarget::Player(id) => TargetRef::Player(id),
            },
            lifelink_controller: entry.lifelink_controller,
            survivor,
        })
        .collect();
    // CR 616.1 + CR 101.4: decisions of different affected players are made
    // in APNAP order. Sorted once here; the remnant keeps the ordered tail, so
    // a resume continues in the same order.
    order_phase_c_legs_by_apnap(state, &mut legs);
    let mut accums = PhaseCAccumulators {
        lifelink_by_source: Vec::new(),
        combat_damage_to_players: Vec::new(),
        source_incarnations: Vec::new(),
        staged_gains: Vec::new(),
    };
    let park = match run_phase_c_legs(state, &mut legs, &mut accums, &mut record.batch_events) {
        PhaseCRunOutcome::Complete => None,
        PhaseCRunOutcome::PausedOnReplacement { index } => Some((index + 1, false)),
        // CR 120.3d + CR 616.1: a `-1/-1` counter placement paused — park
        // exactly like the loss pause (the paused leg's post-counter work
        // rides its combat-flagged counter stash).
        PhaseCRunOutcome::PausedOnCounter { index } => Some((index + 1, false)),
        PhaseCRunOutcome::PausedOnGain { index, pause } => {
            Some((index, pause == GainStagePause::ReplacementChoice))
        }
    };
    if let Some((park_from, on_gain)) = park {
        // CR 510.2 + CR 616.1: a leg's result (or a gain staged ahead of a
        // leg) paused — park the unapplied tail (a paused leg's owed results
        // ride its combat stash; gains already staged ride the remnant) and
        // surface the prompt WITHOUT completing the sub-step, so the turn
        // framework never advances over an open choice.
        let batch_events = record.batch_events.clone();
        record.prevention_tally = prevention_tally;
        record.awaiting_staged_gain = on_gain;
        record.phase_c_remnant = Some(PendingCombatPhaseCRemnant {
            remaining: legs[park_from..].to_vec(),
            lifelink_by_source: accums.lifelink_by_source,
            combat_damage_to_players: accums.combat_damage_to_players,
            source_incarnations: accums.source_incarnations,
            staged_gains: accums.staged_gains,
        });
        state.pending_combat_lifelink = Some(Box::new(record));
        return CombatDamageBatch::Paused {
            events: batch_events,
            waiting_for: Box::new(state.waiting_for.clone()),
        };
    }

    // CR 510.2: the per-player aggregate, built here in exactly the order it has
    // always been emitted and carried to the drain so a pause cannot reorder it
    // ahead of a resumed gain.
    record.damage_to_players = build_combat_damage_to_players_aggregate(
        accums.combat_damage_to_players,
        &accums.source_incarnations,
    );

    // CR 616.1 + CR 702.15e: each surviving source's lifelink gain is its own
    // event. The whole tail — gains, the per-player aggregate, and Phase D's
    // prevention riders — lives in the drain, in
    // the order it has always been emitted, so a CR 616.1 pause can suspend it
    // without reordering it.
    record.remaining = planned_gain_queue(state, accums.lifelink_by_source);
    // CR 101.4: gains staged ahead of a leg settle with the tail's.
    record.staged_gains = accums.staged_gains;
    record.prevention_tally = prevention_tally;
    drain_combat_lifelink(state, record)
}

/// CR 616.1 + CR 702.15e + CR 615.5 + CR 101.4: finish one
/// simultaneous combat-damage batch — stage each remaining lifelink source's
/// life gain as its own event, then settle every staged gain of the batch
/// together (CR 101.4: once all choices are made, the actions happen
/// simultaneously), emit the per-player damage aggregate, fire Phase D's
/// prevention riders.
///
/// The gain is staged through `effects::life::stage_life_gain`, the sibling of
/// the replacement-routed life-change authority. When two or more
/// co-applicable replacements make the ordering material (CR 616.1), it parks
/// `state.pending_replacement`, sets `state.waiting_for` to the ordering prompt,
/// and returns `Err(ReplacementDeferred::ReplacementChoice)` having staged
/// NOTHING. The rest of the batch is parked here so the resume can finish it;
/// dropping it loses 100% of that source's gain, not a portion, plus every
/// later source in the batch.
///
/// Two more choices park the batch here, and the same resume finishes it:
/// a gain's substitute that asks a choice of its own (CR 614.6 + CR 616.2),
/// and a Phase D rider that asks one (CR 615.5 + CR 608.2d). Neither is
/// answered in the player's place.
fn drain_combat_lifelink(
    state: &mut GameState,
    mut record: PendingCombatLifelink,
) -> CombatDamageBatch {
    // CR 104.1: "A game ends immediately when a player wins". An earlier gain's
    // replacement (Nefarious Lich's draw, then Laboratory Maniac's win) can end
    // the game inside the batch; no later source's gain is staged after that.
    while state.game_end.is_none() {
        let Some(gain) = record.remaining.pop_front() else {
            break;
        };
        match stage_combat_lifelink_gain(state, gain, &mut record.batch_events) {
            GainStageOutcome::Staged(staged) => record.staged_gains.push(staged),
            GainStageOutcome::Done => {}
            GainStageOutcome::Paused(pause) => {
                record.awaiting_staged_gain = pause == GainStagePause::ReplacementChoice;
                let waiting_for = state.waiting_for.clone();
                let events = record.batch_events.clone();
                state.pending_combat_lifelink = Some(Box::new(record));
                return CombatDamageBatch::Paused {
                    events,
                    waiting_for: Box::new(waiting_for),
                };
            }
            GainStageOutcome::GameOver => break,
        }
    }
    if state.game_end.is_some() {
        // Damage already dealt still belongs in the event log.
        record.batch_events.append(&mut record.damage_to_players);
        return CombatDamageBatch::Complete(record.batch_events);
    }
    // CR 101.4 + CR 702.15b: every choice of the batch is made; the staged
    // gains now happen simultaneously, settled in stage order before the
    // aggregate. A departed seat's gain was swept by `elimination.rs`; the
    // guard below is a backstop for a record saved before that sweep (a
    // player who left the game gains no life).
    for gain in std::mem::take(&mut record.staged_gains) {
        let departed = state
            .players
            .iter()
            .find(|player| player.id == gain.controller)
            .is_none_or(|player| player.is_eliminated);
        if departed {
            continue;
        }
        crate::game::effects::life::apply_life_gain_after_replacement(
            state,
            ProposedEvent::LifeGain {
                player_id: gain.controller,
                amount: gain.amount,
                applied: Default::default(),
            },
            &mut record.batch_events,
        );
    }
    // CR 510.2: the per-player aggregate, emitted after the gains exactly as it
    // is on a batch that never paused. It records damage already dealt in Phase
    // C. The terminal path above also retains damage already dealt.
    record.batch_events.append(&mut record.damage_to_players);
    // CR 615.5 + CR 615.13: Phase D fires once per shield, and only here — after
    // the last gain, so no rider continuation can ever run while a CR 616.1
    // prompt is open, and after nothing else can stamp `last_effect_count`.
    // CR 104.1: a gain that ended the game ends the batch; no rider runs after it.
    if state.game_end.is_none() {
        let prevention_tally = std::mem::take(&mut record.prevention_tally);
        // CR 101.4 + CR 616.1: rank whole shields by who can be asked,
        // preserving registry and recipient order for ties.
        let rank = apnap_ranker(state);
        let mut groups: Vec<_> = prevention_tally.chunk_by(|a, b| a.key == b.key).collect();
        groups.sort_by_cached_key(|group| {
            if matches!(group[0].key, AppliedReplacementKey::Protection { .. }) {
                return usize::MAX;
            }
            let rid = group[0].key.as_replacement_id();
            let Some(def) = aggregate_prevention_definition(state, rid) else {
                return usize::MAX;
            };
            let Some(rider) = state.replacement_rider(def) else {
                return usize::MAX;
            };
            // A rider whose chooser the scan cannot name keeps its own
            // controller's rank, as before the scan existed.
            match crate::game::resolution_prompt::prevention_rider_choice_rank(
                state,
                rider,
                group.iter().map(|entry| entry.amount).sum(),
                def.damage_source_filter
                    .as_ref()
                    .and_then(shield_specific_source),
                &rank,
            ) {
                usize::MAX => rank(rider.controller),
                chooser_rank => chooser_rank,
            }
        });
        let prevention_tally: Vec<_> = groups.into_iter().flatten().cloned().collect();
        // CR 608.2c + CR 608.2d: a rider that asks its player a choice (Channel
        // Harm's "you may have Channel Harm deal that much damage") stops Phase D:
        // the later shields wait beneath the prompt as the unpublished remainder,
        // and the answer's resume fires them (`resume_combat_damage_replacements`,
        // whose `replacement` is `None` here, re-enters this drain with nothing
        // else owed).
        if let BatchRiderDrainOutcome::Paused { published_entries } =
            fire_batch_prevention_riders(state, &prevention_tally, &mut record.batch_events)
        {
            if state.game_end.is_none() {
                record.prevention_tally = prevention_tally
                    .into_iter()
                    .skip(published_entries)
                    .collect();
                let waiting_for = state.waiting_for.clone();
                let events = record.batch_events.clone();
                state.pending_combat_lifelink = Some(Box::new(record));
                return CombatDamageBatch::Paused {
                    events,
                    waiting_for: Box::new(waiting_for),
                };
            }
        }
    }
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

/// CR 615.13 + CR 608.2c (Matt C2): what one aggregate drainage produced.
/// Either every group dispatched, or a rider left its prompt standing:
/// `published_entries` is the leading entry count fully resolved (events
/// pushed, riders dispatched-or-skipped), so the caller can park the
/// unpublished remainder beneath the prompt instead of firing later groups
/// ahead of it.
pub(crate) enum BatchRiderDrainOutcome {
    Completed,
    Paused { published_entries: usize },
}

fn aggregate_prevention_definition(
    state: &GameState,
    rid: ReplacementId,
) -> Option<&crate::types::ability::ReplacementDefinition> {
    // Floating shields have no host permanent and use the sentinel ObjectId(0).
    if rid.source == ObjectId(0) {
        rid.index_as_usize()
            .and_then(|i| state.pending_damage_replacements.get(i))
    } else {
        state
            .objects
            .get(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get_by_rid(rid))
    }
}

/// CR 615.5 + CR 615.13: After a simultaneous damage batch (combat or
/// noncombat), fire each `Prevention::All` shield's `runtime_execute` rider
/// exactly once against the aggregate prevented amount.
///
/// Each `DamagePrevented` event for the batch is emitted here (the per-source
/// applier suppressed them so the rider sees one un-fragmented amount), ONE per
/// (shield, recipient) — `ShieldKind::PreventionOneShot` included (Matt R15:
/// the applier suppresses its per-portion emit inside a batch). CR 615.13 and
/// CR 120.3 pull in different directions and
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
/// CR 615.5 + CR 608.2c (Matt C2): the drain stops after the first rider that
/// leaves its prompt standing and reports it, so later groups do not execute
/// ahead of the pending prompt; both callers (the combat batch and the
/// noncombat two-phase batch) park the unpublished remainder beneath it.
pub(crate) fn fire_batch_prevention_riders(
    state: &mut GameState,
    prevention_tally: &[PreventedDamageEntry],
    events: &mut Vec<GameEvent>,
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
    let mut published_entries = 0usize;
    for group in prevention_tally.chunk_by(|a, b| a.key == b.key) {
        // CR 104.1: a rider that ended the game ends the batch.
        if state.game_end.is_some() {
            break;
        }
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

        let Some(repl_def) = aggregate_prevention_definition(state, rid) else {
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
        // CR 609.7: A rider referencing the prevented damage's source
        // ("that source's controller" — Deflecting Palm) resolves
        // `PostReplacementSourceController` against the drain's event source. The
        // per-event `execute` path receives it from the Prevented-arm stash; the
        // aggregate path derives it here from the shield's resolved single-object
        // source filter (the chosen damage source).
        let event_source = repl_def
            .damage_source_filter
            .as_ref()
            .and_then(shield_specific_source);
        let runtime = state.replacement_rider(repl_def).cloned();

        // CR 615.13 (Matt R15): one per (shield, recipient) — one-shots
        // publish here like every other shield (the applier suppresses its
        // per-portion emit inside a batch), so one application covering
        // several simultaneous portions is one trigger, not one per portion.
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
            crate::types::ability::PostReplacementContinuation::Resolved(Box::new(runtime)),
        );
        // CR 615.5 + CR 614.6: the rider's "that much" is the amount this application
        // prevented, latched on its drain as a per-event prevention latches it
        // (`replacement::replaced_event_amount`), so a rider that asks a choice first
        // (Channel Harm's "may") still reads it when the answer comes in a later action,
        // after `last_effect_count` has been cleared.
        if let Some(drain) = state
            .active_post_replacement_drains_mut()
            .and_then(crate::types::game_state::PostReplacementDrainStack::resident_mut)
        {
            drain.event_amount = u32::try_from(total_prevented).ok();
            if let Some(source) = event_source {
                drain.event_source = Some(source);
            }
        }
        // CR 608.2c (Matt C2): a rider that left its prompt standing ends the
        // drain here — later groups must not execute ahead of it. The pause is
        // read from the drain's own result, never from `state.waiting_for`: at
        // combat Phase D that still holds the caller's earlier wait (an
        // `AssignCombatDamage` answer, a blocker declaration), not a rider's.
        match replacement::drain_damage_rider(state, events) {
            replacement::DamageRiderDrain::Completed => {}
            replacement::DamageRiderDrain::Paused { .. } => {
                return BatchRiderDrainOutcome::Paused { published_entries };
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

    /// CR 616.1 + CR 101.4: `order_phase_c_legs_by_apnap` keys each leg
    /// by its SURVIVOR's damaged player, not by the assignment's target, and
    /// sorts only player-survivor legs among their own slots (stable).
    #[test]
    fn order_phase_c_legs_by_apnap_keys_by_survivor_player() {
        fn leg(source: u64, target: TargetRef, survivor: Option<TargetRef>) -> PendingCombatLeg {
            let ctx = DamageContext::fallback(ObjectId(source), PlayerId(0));
            PendingCombatLeg {
                ctx: DamageContextSnapshot::from(&ctx),
                source_is_commander: false,
                target,
                lifelink_controller: None,
                survivor: survivor.map(|target| ProposedEvent::Damage {
                    source_id: ObjectId(source),
                    target,
                    amount: 1,
                    is_combat: true,
                    source_name: None,
                    source_incarnation: None,
                    applied: Default::default(),
                }),
            }
        }
        fn sources(legs: &[PendingCombatLeg]) -> Vec<u64> {
            legs.iter().map(|leg| leg.ctx.source_id.0).collect()
        }
        let p = |n: u8| TargetRef::Player(PlayerId(n));
        let creature = TargetRef::Object(ObjectId(900));

        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        state.active_player = PlayerId(0);

        // (a) player legs reorder among their slots: P2, P1 -> P1, P2; the
        // creature leg between them keeps slot 1.
        let mut legs = vec![
            leg(1, p(2), Some(p(2))),
            leg(2, creature.clone(), Some(creature.clone())),
            leg(3, p(1), Some(p(1))),
        ];
        order_phase_c_legs_by_apnap(&state, &mut legs);
        assert_eq!(
            sources(&legs),
            vec![3, 2, 1],
            "(a) reordered (input was [1, 2, 3])"
        );

        // (b) assignment at a creature, survivor redirected to P1: ordered by
        // P1 (ahead of the P2 leg). (c) assignment at P1, survivor at an
        // object: keeps its slot.
        let mut legs = vec![
            leg(10, p(2), Some(p(2))),
            leg(11, p(1), Some(creature.clone())),
            leg(12, creature.clone(), Some(p(1))),
        ];
        order_phase_c_legs_by_apnap(&state, &mut legs);
        assert_eq!(
            sources(&legs),
            vec![12, 11, 10],
            "(b) the redirected survivor sorts by P1; (c) the object survivor stays in slot 1"
        );

        // (d) a `None` survivor (fully prevented) keeps its slot even though
        // its assignment targets the earliest APNAP player.
        let mut legs = vec![
            leg(20, p(2), Some(p(2))),
            leg(21, p(0), None),
            leg(22, p(1), Some(p(1))),
        ];
        order_phase_c_legs_by_apnap(&state, &mut legs);
        assert_eq!(sources(&legs), vec![22, 21, 20], "(d)");

        // (f) an eliminated player and an unknown player sort last; the
        // living P2 goes first, then the two last-rank legs in declaration
        // order.
        let mut eliminated = state.clone();
        eliminated
            .players
            .iter_mut()
            .find(|player| player.id == PlayerId(1))
            .unwrap()
            .is_eliminated = true;
        let mut legs = vec![
            leg(30, p(1), Some(p(1))),
            leg(31, p(9), Some(p(9))),
            leg(32, p(2), Some(p(2))),
        ];
        order_phase_c_legs_by_apnap(&eliminated, &mut legs);
        assert_eq!(sources(&legs), vec![32, 30, 31], "(f)");

        // (e) equal ranks keep declaration order: two teammates of one
        // shared-turn team (CR 805) share an APNAP choice group.
        let mut team = GameState::new(
            crate::types::format::FormatConfig::two_headed_giant(),
            4,
            42,
        );
        team.active_player = PlayerId(0);
        let rank = |n: u8| crate::game::topology::apnap_team_rank(&team, PlayerId(n));
        let mate = (1..4)
            .find(|&n| rank(n) == rank(0))
            .expect("reach-guard: P0 has a teammate sharing its APNAP rank");
        let other = (1..4)
            .find(|&n| rank(n) > rank(0))
            .expect("reach-guard: an opposing seat ranks after P0's team");
        let mut legs = vec![
            leg(40, p(mate), Some(p(mate))),
            leg(41, p(other), Some(p(other))),
            leg(42, p(0), Some(p(0))),
        ];
        order_phase_c_legs_by_apnap(&team, &mut legs);
        assert_eq!(
            sources(&legs),
            vec![40, 42, 41],
            "(e) the teammates keep declaration order; the other team moves last"
        );
    }

    const VORINCLEX_TEXT: &str = "Trample, haste\nIf you would put one or more counters on a permanent or player, put twice that many of each of those kinds of counters on that permanent or player instead.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";
    const GAIN_PLUS_ONE: &str = "If you would gain life, you gain that much life plus 1 instead.";
    const GAIN_TWICE: &str = "If you would gain life, you gain twice that much life instead.";

    /// Two competing life-gain replacements: the gain's own pipeline can open
    /// a CR 616.1 ordering choice for `player`.
    fn add_competing_gain_replacements(scenario: &mut GameScenario, player: PlayerId) {
        scenario.add_enchantment_from_oracle(player, "Plus One", GAIN_PLUS_ONE);
        scenario.add_enchantment_from_oracle(player, "Twice", GAIN_TWICE);
    }

    /// A synthetic Phase C leg: `survivor` deals `amount` to `survivor_target`
    /// (`None` = fully prevented); `lifelink` is the gaining controller.
    fn phase_c_leg(
        source: u64,
        controller: PlayerId,
        target: TargetRef,
        survivor_target: Option<TargetRef>,
        amount: u32,
        lifelink: Option<PlayerId>,
        infect: bool,
    ) -> PendingCombatLeg {
        let mut ctx = DamageContext::fallback(ObjectId(source), controller);
        ctx.has_infect = infect;
        PendingCombatLeg {
            ctx: DamageContextSnapshot::from(&ctx),
            source_is_commander: false,
            target,
            lifelink_controller: lifelink,
            survivor: survivor_target.map(|target| ProposedEvent::Damage {
                source_id: ObjectId(source),
                target,
                amount,
                is_combat: true,
                source_name: None,
                source_incarnation: None,
                applied: Default::default(),
            }),
        }
    }

    fn empty_accums() -> PhaseCAccumulators {
        PhaseCAccumulators {
            lifelink_by_source: Vec::new(),
            combat_damage_to_players: Vec::new(),
            source_incarnations: Vec::new(),
            staged_gains: Vec::new(),
        }
    }

    /// CR 101.4 + CR 616.1: `take_staged_answer` claims an answered life gain
    /// only while the parked batch awaits one of its own gains: (a) no record,
    /// or a record not awaiting, hands the event back to be settled normally
    /// (a later unrelated life-gain answer is never claimed); (b) awaiting
    /// with a Phase C remnant: the gain joins the remnant's staged gains;
    /// (c) awaiting without a remnant: the record's. Each claim clears the
    /// marker, so a second answer is handed back.
    #[test]
    fn take_staged_answer_claims_only_an_awaited_gain() {
        let gain = |amount| ProposedEvent::LifeGain {
            player_id: P1,
            amount,
            applied: Default::default(),
        };
        let record = |awaiting: bool, remnant: bool| PendingCombatLifelink {
            remaining: Default::default(),
            staged_gains: Vec::new(),
            awaiting_staged_gain: awaiting,
            batch_events: Vec::new(),
            damage_to_players: Vec::new(),
            prevention_tally: Vec::new(),
            sub_step: CombatDamageSubStep::Regular,
            replacement: None,
            phase_c_remnant: remnant.then(|| PendingCombatPhaseCRemnant {
                remaining: Vec::new(),
                lifelink_by_source: Vec::new(),
                combat_damage_to_players: Vec::new(),
                source_incarnations: Vec::new(),
                staged_gains: Vec::new(),
            }),
        };
        let staged = |amount| PendingLifelinkGain {
            controller: P1,
            amount,
        };

        // (a)
        let mut state = GameState::new_two_player(42);
        assert_eq!(take_staged_answer(&mut state, gain(3)), Some(gain(3)));
        state.pending_combat_lifelink = Some(Box::new(record(false, true)));
        assert_eq!(take_staged_answer(&mut state, gain(3)), Some(gain(3)));
        assert!(state
            .pending_combat_lifelink
            .as_ref()
            .unwrap()
            .phase_c_remnant
            .as_ref()
            .unwrap()
            .staged_gains
            .is_empty());

        // (b)
        state.pending_combat_lifelink = Some(Box::new(record(true, true)));
        assert_eq!(take_staged_answer(&mut state, gain(3)), None);
        let parked = state.pending_combat_lifelink.as_ref().unwrap();
        assert!(!parked.awaiting_staged_gain, "(b) marker cleared");
        assert_eq!(
            parked.phase_c_remnant.as_ref().unwrap().staged_gains,
            vec![staged(3)]
        );
        assert!(parked.staged_gains.is_empty());
        assert_eq!(
            take_staged_answer(&mut state, gain(4)),
            Some(gain(4)),
            "(b) a second answer is not claimed"
        );

        // (c)
        state.pending_combat_lifelink = Some(Box::new(record(true, false)));
        assert_eq!(take_staged_answer(&mut state, gain(5)), None);
        assert_eq!(
            state.pending_combat_lifelink.as_ref().unwrap().staged_gains,
            vec![staged(5)]
        );
    }

    fn paused_gain(state: &GameState) -> Option<(PlayerId, u32)> {
        match state.pending_replacement.as_ref().map(|p| &p.proposed) {
            Some(ProposedEvent::LifeGain {
                player_id, amount, ..
            }) => Some((*player_id, *amount)),
            _ => None,
        }
    }

    /// CR 616.1 + CR 101.4 + CR 101.4c + CR 805.6: staging lifelink gains
    /// ahead of a leg. (a) a redirected survivor (assignment at a creature,
    /// survivor at P1) totals by the survivor amount, merged with the
    /// already-folded sum, and is staged for the gaining player (it pauses on
    /// that player's choice, nothing applied); (b) a `None` survivor and a
    /// phased-out target add 0; (c) on a shared-turn team the active team's
    /// gain is staged ahead of a leg at its own team (equal rank, ungated) but
    /// not ahead of a non-prompting opposing-team leg; (d) an eliminated controller ranks last and is
    /// never staged ahead of a living player's leg; (e) a gain that cannot
    /// prompt is staged without a pause and WITHOUT writing the board when its
    /// owner is the damaged player, but not ahead of another player's
    /// non-prompting leg.
    #[test]
    fn stage_earlier_apnap_lifelink_stages_by_gaining_player() {
        let p = |n: u8| TargetRef::Player(PlayerId(n));
        let creature = TargetRef::Object(ObjectId(900));

        // Shared 3-player board: P0 active, P0 and P1 each own two competing
        // gain replacements; the Vorinclex pair makes P0's infect poison
        // results choice-capable.
        let board = || {
            let mut scenario = GameScenario::new_n_player(3, 42);
            add_competing_gain_replacements(&mut scenario, P0);
            add_competing_gain_replacements(&mut scenario, P1);
            for owner in [P0, P1] {
                scenario
                    .add_creature(owner, "Vorinclex, Monstrous Raider", 6, 6)
                    .from_oracle_text_with_keywords(&["Trample", "Haste"], VORINCLEX_TEXT);
            }
            let mut runner = scenario.build();
            runner.state_mut().active_player = P0;
            runner
        };

        // Reach-guards: both owners' gains are choice-capable, the infect leg
        // at P2 can prompt, and P2 ranks after P0 and P1.
        let runner = board();
        let state = runner.state();
        assert!(crate::game::effects::life::life_gain_may_prompt(
            state, P0, 2
        ));
        assert!(crate::game::effects::life::life_gain_may_prompt(
            state, P1, 2
        ));
        let infect_leg = phase_c_leg(11, P0, p(2), Some(p(2)), 3, None, true);
        assert!(combat_leg_may_prompt(state, &infect_leg));
        let rank = |n: u8| crate::game::topology::apnap_team_rank(state, PlayerId(n));
        assert!(rank(0) < rank(1) && rank(1) < rank(2));

        // (a) the P0 lifelinker's survivor was redirected from a creature to
        // P1 (amount 2); 1 more was already folded: P0's gain is 3, settled
        // (and paused on P0's own choice) before the P2 leg.
        let mut runner = board();
        let state = runner.state_mut();
        let mut legs = vec![
            phase_c_leg(11, P0, p(2), Some(p(2)), 3, None, true),
            phase_c_leg(10, P0, creature.clone(), Some(p(1)), 2, Some(P0), false),
        ];
        let mut accums = empty_accums();
        accums.lifelink_by_source.push((ObjectId(10), P0, 1));
        let mut events = Vec::new();
        let rank2 = crate::game::topology::apnap_team_rank(state, PlayerId(2));
        assert!(
            stage_earlier_apnap_lifelink(state, &mut legs, 0, rank2, &mut accums, &mut events)
                .is_some()
        );
        assert_eq!(
            paused_gain(state),
            Some((P0, 3)),
            "(a) 1 folded + 2 planned"
        );
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { player, .. } if player == P0
        ));
        assert_eq!(legs[1].lifelink_controller, None, "(a) marker cleared");
        assert!(accums.lifelink_by_source.is_empty(), "(a) sum removed");
        assert_eq!(state.players[0].life, 20, "(a) nothing applied");

        // (b) a fully prevented (`None`) leg and a leg at a phased-out player
        // plan 0: nothing is settled and nothing is cleared.
        let mut runner = board();
        let state = runner.state_mut();
        state
            .players
            .iter_mut()
            .find(|player| player.id == P1)
            .unwrap()
            .status = crate::types::player::PlayerStatus::PhasedOut;
        let mut legs = vec![
            phase_c_leg(11, P0, p(2), Some(p(2)), 3, None, true),
            phase_c_leg(20, P0, p(1), None, 0, Some(P0), false),
            phase_c_leg(21, P0, p(1), Some(p(1)), 4, Some(P0), false),
        ];
        assert_eq!(planned_leg_amount(state, &legs[1]), 0);
        assert_eq!(planned_leg_amount(state, &legs[2]), 0);
        let mut accums = empty_accums();
        let mut events = Vec::new();
        let rank2 = crate::game::topology::apnap_team_rank(state, PlayerId(2));
        assert!(
            stage_earlier_apnap_lifelink(state, &mut legs, 0, rank2, &mut accums, &mut events)
                .is_none()
        );
        assert_eq!(paused_gain(state), None, "(b)");
        assert_eq!(legs[1].lifelink_controller, Some(P0));
        assert_eq!(legs[2].lifelink_controller, Some(P0));

        // (d) an eliminated P1's gain ranks last: never staged ahead of the
        // P2 leg. Paired with the living-P1 control, which is staged.
        for eliminated in [false, true] {
            let mut runner = board();
            let state = runner.state_mut();
            state
                .players
                .iter_mut()
                .find(|player| player.id == P1)
                .unwrap()
                .is_eliminated = eliminated;
            let mut legs = vec![
                phase_c_leg(11, P0, p(2), Some(p(2)), 3, None, true),
                phase_c_leg(
                    30,
                    P1,
                    creature.clone(),
                    Some(creature.clone()),
                    2,
                    Some(P1),
                    false,
                ),
            ];
            let mut accums = empty_accums();
            let mut events = Vec::new();
            let rank2 = crate::game::topology::apnap_team_rank(state, PlayerId(2));
            let paused =
                stage_earlier_apnap_lifelink(state, &mut legs, 0, rank2, &mut accums, &mut events);
            assert_eq!(paused.is_some(), !eliminated, "(d) eliminated={eliminated}");
            assert_eq!(
                legs[1].lifelink_controller,
                (eliminated).then_some(P1),
                "(d) eliminated={eliminated}"
            );
        }

        // (c) CR 805.6: Two-Headed Giant, no Vorinclex (the infect leg cannot
        // prompt). The active team's teammate's gain is NOT staged ahead of
        // the non-prompting opposing-team leg (strictly earlier ranks are
        // gated on the leg's prompt), but IS staged ahead of a leg at a player
        // of the gainer's own team (equal rank, CR 101.4c, ungated).
        let mut team = GameScenario::new_with_format(
            crate::types::format::FormatConfig::two_headed_giant(),
            4,
            42,
        );
        team.state.active_player = P0;
        let rank_of =
            |state: &GameState, n: u8| crate::game::topology::apnap_team_rank(state, PlayerId(n));
        let mate = (1..4)
            .find(|&n| rank_of(&team.state, n) == rank_of(&team.state, 0))
            .expect("reach-guard: P0 has a teammate sharing its APNAP rank");
        let other = (1..4)
            .find(|&n| rank_of(&team.state, n) > rank_of(&team.state, 0))
            .expect("reach-guard: an opposing seat ranks after P0's team");
        add_competing_gain_replacements(&mut team, PlayerId(mate));
        let runner = team.build();
        assert!(crate::game::effects::life::life_gain_may_prompt(
            runner.state(),
            PlayerId(mate),
            2
        ));
        for (leg_player, expect_settled) in [(other, false), (0, true)] {
            let mut state = runner.state().clone();
            let mut legs = vec![
                phase_c_leg(50, P0, p(leg_player), Some(p(leg_player)), 3, None, true),
                phase_c_leg(
                    51,
                    PlayerId(mate),
                    creature.clone(),
                    Some(creature.clone()),
                    2,
                    Some(PlayerId(mate)),
                    false,
                ),
            ];
            let mut accums = empty_accums();
            let mut events = Vec::new();
            let leg_rank = rank_of(&state, leg_player);
            let paused = stage_earlier_apnap_lifelink(
                &mut state,
                &mut legs,
                0,
                leg_rank,
                &mut accums,
                &mut events,
            );
            assert_eq!(paused.is_some(), expect_settled, "(c) leg at P{leg_player}");
            assert_eq!(
                paused_gain(&state),
                expect_settled.then_some((PlayerId(mate), 2)),
                "(c) leg at P{leg_player}"
            );
        }

        // (e) no replacement at all: P1's lifelink blocker (source 61) hits a
        // creature while P0's plain attacker (source 60) hits P1; P0's own
        // lifelinker (source 62) hits a creature too. Neither gain can prompt
        // and the P1 leg cannot prompt. P1's gain (the damaged player's own,
        // equal rank) is staged: recorded, marker cleared, life unchanged.
        // P0's gain (strictly earlier rank, but the leg cannot prompt) stays
        // with its leg for the tail.
        let mut runner = GameScenario::new().build();
        let state = runner.state_mut();
        state.active_player = P0;
        let mut legs = vec![
            phase_c_leg(60, P0, p(1), Some(p(1)), 3, None, false),
            phase_c_leg(
                61,
                P1,
                creature.clone(),
                Some(creature.clone()),
                2,
                Some(P1),
                false,
            ),
            phase_c_leg(
                62,
                P0,
                creature.clone(),
                Some(creature.clone()),
                2,
                Some(P0),
                false,
            ),
        ];
        assert!(!combat_leg_may_prompt(state, &legs[0]), "reach-guard");
        let mut accums = empty_accums();
        let mut events = Vec::new();
        let rank1 = crate::game::topology::apnap_team_rank(state, P1);
        assert!(
            stage_earlier_apnap_lifelink(state, &mut legs, 0, rank1, &mut accums, &mut events)
                .is_none()
        );
        assert_eq!(
            accums.staged_gains,
            vec![PendingLifelinkGain {
                controller: P1,
                amount: 2
            }],
            "(e) the damaged player's own gain is staged"
        );
        assert_eq!(legs[1].lifelink_controller, None, "(e) marker cleared");
        assert_eq!(
            legs[2].lifelink_controller,
            Some(P0),
            "(e) P0 keeps its slot"
        );
        assert_eq!(state.players[1].life, 20, "(e) staged, not applied");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, GameEvent::LifeChanged { .. })),
            "(e) no life change before the batch settles"
        );
    }

    /// CR 616.1 + CR 101.4 + CR 805.6: Two-Headed Giant through
    /// `run_phase_c_legs`. P0 (active) and its teammate share one APNAP rank;
    /// the opposing team ranks after. The teammate's lifelink gain can prompt
    /// (two competing gain replacements), and P0's infect leg can prompt
    /// wherever it lands (P0's Vorinclex doubles, the opposing Vorinclex
    /// halves). (a) a leg at the opposing team: the run pauses on the
    /// teammate's gain before the leg; (b) a leg at P0's own team (equal rank,
    /// CR 101.4c): the teammate's gain is staged before that leg too, so the
    /// run pauses on the gain and the leg has not applied; (c) the tail queue
    /// orders the choice-capable gains by team rank, teammates keeping their
    /// input order.
    #[test]
    fn two_headed_giant_phase_c_stages_gains_by_team_rank() {
        let p = |n: u8| TargetRef::Player(PlayerId(n));
        let creature = TargetRef::Object(ObjectId(900));
        let mut team = GameScenario::new_with_format(
            crate::types::format::FormatConfig::two_headed_giant(),
            4,
            42,
        );
        team.state.active_player = P0;
        let rank_of =
            |state: &GameState, n: u8| crate::game::topology::apnap_team_rank(state, PlayerId(n));
        let mate = (1..4)
            .find(|&n| rank_of(&team.state, n) == rank_of(&team.state, 0))
            .expect("reach-guard: P0 has a teammate sharing its APNAP rank");
        let other = (1..4)
            .find(|&n| rank_of(&team.state, n) > rank_of(&team.state, 0))
            .expect("reach-guard: an opposing seat ranks after P0's team");
        add_competing_gain_replacements(&mut team, P0);
        add_competing_gain_replacements(&mut team, PlayerId(mate));
        add_competing_gain_replacements(&mut team, PlayerId(other));
        for owner in [P0, PlayerId(other)] {
            team.add_creature(owner, "Vorinclex, Monstrous Raider", 6, 6)
                .from_oracle_text_with_keywords(&["Trample", "Haste"], VORINCLEX_TEXT);
        }
        let runner = team.build();
        let legs_at = |leg_player: u8| {
            vec![
                phase_c_leg(50, P0, p(leg_player), Some(p(leg_player)), 3, None, true),
                phase_c_leg(
                    51,
                    PlayerId(mate),
                    creature.clone(),
                    Some(creature.clone()),
                    2,
                    Some(PlayerId(mate)),
                    false,
                ),
            ]
        };
        {
            let state = runner.state();
            assert!(crate::game::effects::life::life_gain_may_prompt(
                state,
                PlayerId(mate),
                2
            ));
            for leg_player in [other, 0] {
                assert!(
                    combat_leg_may_prompt(state, &legs_at(leg_player)[0]),
                    "reach-guard: the infect leg at P{leg_player} can prompt"
                );
            }
        }

        // (a) other-team leg: the teammate's gain is staged first and pauses.
        let mut state = runner.state().clone();
        let mut legs = legs_at(other);
        let mut accums = empty_accums();
        let mut events = Vec::new();
        let outcome = run_phase_c_legs(&mut state, &mut legs, &mut accums, &mut events);
        assert!(
            matches!(outcome, PhaseCRunOutcome::PausedOnGain { index: 0, .. }),
            "(a) staged gain pause before the leg"
        );
        assert_eq!(paused_gain(&state), Some((PlayerId(mate), 2)), "(a)");
        assert_eq!(legs[1].lifelink_controller, None, "(a) staged source");
        assert_eq!(state.players[other as usize].poison_counters, 0);

        // (b) own-team leg (equal rank): the teammate's gain is staged first
        // too; the leg has not applied (no poison yet).
        let mut state = runner.state().clone();
        let mut legs = legs_at(0);
        let mut accums = empty_accums();
        let mut events = Vec::new();
        let outcome = run_phase_c_legs(&mut state, &mut legs, &mut accums, &mut events);
        assert!(
            matches!(outcome, PhaseCRunOutcome::PausedOnGain { index: 0, .. }),
            "(b) the equal-rank gain is staged before the leg"
        );
        assert_eq!(paused_gain(&state), Some((PlayerId(mate), 2)), "(b)");
        assert_eq!(legs[1].lifelink_controller, None, "(b) staged source");
        assert_eq!(state.players[0].poison_counters, 0, "(b) leg not applied");

        // (c) tail order: the opposing team's gain goes last; the two
        // teammates (equal rank) keep their input order either way round.
        let state = runner.state();
        let order = |input: Vec<(ObjectId, PlayerId, u32)>| -> Vec<PlayerId> {
            planned_gain_queue(state, input)
                .iter()
                .map(|gain| gain.controller)
                .collect()
        };
        assert_eq!(
            order(vec![
                (ObjectId(1), PlayerId(other), 2),
                (ObjectId(2), PlayerId(mate), 2),
                (ObjectId(3), P0, 2),
            ]),
            vec![PlayerId(mate), P0, PlayerId(other)],
            "(c)"
        );
        assert_eq!(
            order(vec![
                (ObjectId(3), P0, 2),
                (ObjectId(1), PlayerId(other), 2),
                (ObjectId(2), PlayerId(mate), 2),
            ]),
            vec![P0, PlayerId(mate), PlayerId(other)],
            "(c) reversed teammates"
        );
    }

    /// CR 616.1 + CR 101.4: the tail gain queue reorders only the gains whose
    /// own pipeline can open a choice, by the gaining player's APNAP rank;
    /// a gain that cannot prompt keeps its slot.
    #[test]
    fn planned_gain_queue_orders_only_choice_capable_gains() {
        let mut scenario = GameScenario::new_n_player(3, 42);
        add_competing_gain_replacements(&mut scenario, P1);
        add_competing_gain_replacements(&mut scenario, PlayerId(2));
        let mut runner = scenario.build();
        runner.state_mut().active_player = P0;
        let state = runner.state();
        // Reach-guard: P1 and P2 can prompt; P0 cannot (no replacements).
        assert!(crate::game::effects::life::life_gain_may_prompt(
            state, P1, 2
        ));
        assert!(crate::game::effects::life::life_gain_may_prompt(
            state,
            PlayerId(2),
            2
        ));
        assert!(!crate::game::effects::life::life_gain_may_prompt(
            state, P0, 3
        ));
        let queue = planned_gain_queue(
            state,
            vec![
                (ObjectId(1), PlayerId(2), 2),
                (ObjectId(2), P0, 3),
                (ObjectId(3), P1, 2),
            ],
        );
        let order: Vec<(PlayerId, u32)> = queue.iter().map(|g| (g.controller, g.amount)).collect();
        assert_eq!(order, vec![(P1, 2), (P0, 3), (PlayerId(2), 2)]);
    }

    /// CR 616.1 + CR 120.3: the leg-side gate. Without competing counter
    /// replacements no leg can prompt; under the Vorinclex pair only the
    /// infect leg at a player with a nonzero amount can (the positive
    /// control), never a plain life-loss leg, a 0-amount leg or a leg at an
    /// object. A phased-out player has no result.
    #[test]
    fn combat_leg_may_prompt_reads_the_result_events() {
        let p1 = TargetRef::Player(P1);
        let object = TargetRef::Object(ObjectId(901));
        for pair in [false, true] {
            let mut scenario = GameScenario::new();
            if pair {
                for owner in [P0, P1] {
                    scenario
                        .add_creature(owner, "Vorinclex, Monstrous Raider", 6, 6)
                        .from_oracle_text_with_keywords(&["Trample", "Haste"], VORINCLEX_TEXT);
                }
            }
            let mut runner = scenario.build();
            let probe = |state: &GameState, infect: bool, target: TargetRef, amount: u32| {
                combat_leg_may_prompt(
                    state,
                    &phase_c_leg(900, P0, target.clone(), Some(target), amount, None, infect),
                )
            };
            let state = runner.state();
            assert!(
                !probe(state, false, p1.clone(), 3),
                "plain loss, pair={pair}"
            );
            assert_eq!(
                probe(state, true, p1.clone(), 3),
                pair,
                "infect, pair={pair}"
            );
            assert!(!probe(state, true, p1.clone(), 0), "amount 0, pair={pair}");
            assert!(
                !probe(state, true, object.clone(), 3),
                "object, pair={pair}"
            );
            assert!(
                !combat_leg_may_prompt(
                    state,
                    &phase_c_leg(900, P0, p1.clone(), None, 3, None, true)
                ),
                "None survivor, pair={pair}"
            );
            // CR 120.3g: a non-infect creature with toxic owes a second poison
            // result, which the pair makes choice-capable.
            let mut toxic = phase_c_leg(900, P0, p1.clone(), Some(p1.clone()), 3, None, false);
            toxic.ctx.source_is_creature = true;
            toxic.ctx.combat_damage_poison = 2;
            assert_eq!(
                combat_leg_may_prompt(state, &toxic),
                pair,
                "toxic, pair={pair}"
            );
            if pair {
                runner
                    .state_mut()
                    .players
                    .iter_mut()
                    .find(|player| player.id == P1)
                    .unwrap()
                    .status = crate::types::player::PlayerStatus::PhasedOut;
                assert!(!probe(runner.state(), true, p1.clone(), 3), "phased out");
            }
        }
    }

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

    /// CR 120.6 + CR 613.11: the survival estimate reads the lethal-damage basis. With a
    /// "lethal damage dealt to creatures you control is determined by their power" static on
    /// P1's side, P1's 3/1 blocker survives the 1/1's 1 damage (power 3); without it, it dies.
    #[test]
    fn assess_combat_impact_reads_the_lethal_damage_basis() {
        for (with_static, blocker_survives) in [(true, true), (false, false)] {
            let mut state = setup();
            let attacker = create_creature(&mut state, PlayerId(0), "Attacker", 1, 1);
            let blocker = create_creature(&mut state, PlayerId(1), "Blocker", 3, 1);
            if with_static {
                let carrier = create_creature(
                    &mut state,
                    PlayerId(1),
                    "Zilortha, Strength Incarnate",
                    7,
                    3,
                );
                let def = StaticDefinition::new(
                    crate::types::statics::StaticMode::LethalDamageDeterminedByPower,
                )
                .affected(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .into(),
                );
                let obj = state.objects.get_mut(&carrier).unwrap();
                obj.static_definitions.push(def.clone());
                Arc::make_mut(&mut obj.base_static_definitions).push(def);
            }
            setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);
            let attacker_ref = current_incarnation(&state, attacker);
            let blocker_ref = current_incarnation(&state, blocker);

            let CombatImpact::Fixed { survival, .. } =
                assess_combat_impact(&state, attacker_ref, blocker_ref)
            else {
                panic!("a plain one-on-one exchange is fixed (with_static: {with_static})");
            };
            assert_eq!(
                survival.blocker_survives, blocker_survives,
                "with_static: {with_static}"
            );
            assert!(!survival.attacker_survives, "3 damage kills the 1/1");
        }
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

    /// CR 510.3 + CR 510.4: resolve BOTH combat damage steps of a first-strike or
    /// double-strike combat. `resolve_combat_damage` performs one step per call and
    /// returns the active player's priority window after the first-strike step;
    /// while that window is open with an empty stack, calling it again models every
    /// player passing it (the all-pass gate in `priority.rs` re-enters the function).
    /// Tests that assert the window itself make the two calls explicitly instead.
    fn resolve_both_combat_damage_steps(state: &mut GameState, events: &mut Vec<GameEvent>) {
        let mut waiting = resolve_combat_damage(state, events);
        while matches!(waiting, Some(WaitingFor::Priority { .. }))
            && between_combat_damage_steps(state)
        {
            waiting = resolve_combat_damage(state, events);
        }
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
                resolve_both_combat_damage_steps(&mut state, &mut events);

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

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Object(target),
                amount: 2,
                is_combat: true,
                ..
            } if *source_id == attacker && *target == blocker
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Object(target),
                amount: 2,
                is_combat: true,
                ..
            } if *source_id == blocker && *target == attacker
        )));
        assert_eq!(
            state.objects[&attacker].zone,
            crate::types::zones::Zone::Graveyard,
            "CR 704.5g: the attacker is destroyed by lethal combat damage"
        );
        assert_eq!(
            state.objects[&blocker].zone,
            crate::types::zones::Zone::Graveyard,
            "CR 704.5g: the blocker is destroyed by lethal combat damage"
        );
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
        let first = resolve_combat_damage(&mut state, &mut events);

        // CR 510.3: the first-strike step ends with the priority window.
        assert!(
            matches!(first, Some(WaitingFor::Priority { .. })),
            "the first-strike step ends with a priority window, got {first:?}"
        );
        assert!(between_combat_damage_steps(&state));
        // First strike dealt 3 damage (lethal) to blocker
        // SBAs ran between steps -- blocker should have been destroyed
        // Blocker can't deal damage back in regular step (dead)
        // Attacker should have 0 damage
        assert_eq!(state.objects[&attacker].damage_marked, 0);
        // Blocker should be in graveyard (SBAs ran between steps)
        assert!(!state.battlefield.contains(&blocker));

        // The regular step (one pass round later) adds nothing: the dead blocker
        // deals no damage and the first striker does not strike again.
        let second = resolve_combat_damage(&mut state, &mut events);
        assert!(second.is_none(), "the regular step completes: {second:?}");
        assert!(state.combat.as_ref().unwrap().regular_damage_done);
        assert_eq!(state.objects[&attacker].damage_marked, 0);
    }

    /// CR 510.4 + CR 702.7b: Youthful Knight (2/1, first strike) blocked by Goblin
    /// Piker (2/1). The Knight's first-strike damage kills the Piker, so the Piker
    /// never deals its regular-step damage and the Knight survives; the Knight does
    /// not deal damage again in the second step.
    #[test]
    fn youthful_knight_kills_a_toughness_one_blocker_before_it_strikes_back() {
        let mut state = setup();
        let knight = create_creature(&mut state, PlayerId(0), "Youthful Knight", 2, 1);
        state
            .objects
            .get_mut(&knight)
            .unwrap()
            .keywords
            .push(Keyword::FirstStrike);
        let piker = create_creature(&mut state, PlayerId(1), "Goblin Piker", 2, 1);
        setup_combat(&mut state, vec![knight], vec![(knight, vec![piker])]);

        let mut events = Vec::new();
        let first = resolve_combat_damage(&mut state, &mut events);

        assert!(matches!(first, Some(WaitingFor::Priority { .. })));
        assert!(
            !state.battlefield.contains(&piker),
            "reach guard: the Piker died"
        );
        assert_eq!(state.objects[&knight].damage_marked, 0);

        let second = resolve_combat_damage(&mut state, &mut events);

        assert!(second.is_none());
        assert_eq!(
            state.objects[&knight].damage_marked, 0,
            "the dead Piker deals no regular-step damage"
        );
        assert_eq!(
            state.players[1].life, 20,
            "a blocked attacker deals no damage to the player"
        );
    }

    /// CR 510.4 + CR 702.4b + CR 510.3: Fencing Ace (1/1, double strike) attacks an
    /// unblocked player. The first combat damage step deals 1 and ends with the
    /// active player's priority window; the regular step, one pass round later,
    /// deals 1 more.
    #[test]
    fn double_strike_deals_damage_twice() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Fencing Ace", 1, 1);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::DoubleStrike);
        setup_combat(&mut state, vec![attacker], vec![]);
        // Stale priority bookkeeping the window must clear (CR 510.3).
        state.priority_player = PlayerId(1);
        state.priority_passes.insert(PlayerId(1));

        let mut events = Vec::new();
        let first = resolve_combat_damage(&mut state, &mut events);

        // First step: 1 damage, then the active player's priority window.
        assert!(
            matches!(first, Some(WaitingFor::Priority { player }) if player == PlayerId(0)),
            "CR 510.3: the first combat damage step ends with the active player's priority, got {first:?}"
        );
        assert_eq!(state.players[1].life, 19, "the first-strike hit landed");
        let combat = state.combat.as_ref().unwrap();
        assert!(combat.first_strike_done && !combat.regular_damage_done);
        assert!(state.stack.is_empty());
        assert!(between_combat_damage_steps(&state));
        assert_eq!(state.priority_player, PlayerId(0));
        assert!(state.priority_passes.is_empty());

        // All players pass: the gate re-enters for the regular step.
        let second = resolve_combat_damage(&mut state, &mut events);

        assert!(second.is_none(), "the regular step completes: {second:?}");
        assert_eq!(state.players[1].life, 18, "1 + 1 = 2 damage to the player");
        assert!(state.combat.as_ref().unwrap().regular_damage_done);
        assert!(!between_combat_damage_steps(&state));
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
        assert!(state.objects[&attacker]
            .keywords
            .contains(&Keyword::Trample));
        assert!(state.objects[&attacker]
            .keywords
            .contains(&Keyword::Deathtouch));
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

        // Damage events retain the assignment before the resulting SBAs move the blockers.
        for blocker in [blocker1, blocker2] {
            assert!(events.iter().any(|event| matches!(
                event,
                GameEvent::DamageDealt {
                    source_id,
                    target: TargetRef::Object(target),
                    amount: 1,
                    is_combat: true,
                    ..
                } if *source_id == attacker && *target == blocker
            )));
            assert_eq!(
                state.objects[&blocker].zone,
                crate::types::zones::Zone::Graveyard,
                "CR 702.2b + CR 704.5h: one damage from a deathtouch source destroys the blocker"
            );
            assert!(events.iter().any(|event| matches!(
                event,
                GameEvent::DamageDealt {
                    source_id,
                    target: TargetRef::Object(target),
                    amount: 2,
                    is_combat: true,
                    ..
                } if *source_id == blocker && *target == attacker
            )));
        }
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Player(player),
                amount: 3,
                is_combat: true,
                ..
            } if *source_id == attacker && *player == PlayerId(1)
        )));
        assert_eq!(
            state.objects[&attacker].zone,
            crate::types::zones::Zone::Battlefield
        );
        assert_eq!(state.objects[&attacker].damage_marked, 4);
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
        // CR 120.8: a fully prevented hit deals no damage, so the aggregate
        // needs a hit that is actually dealt — the lifelinker attacks a second
        // defender (P2) who has no shield, while the Ogre's hit on P1 is
        // prevented by P1's Inkshield.
        let mut state = GameState::new(crate::types::format::FormatConfig::free_for_all(), 3, 42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        install_inkshield(&mut state, PlayerId(1));
        let prevented = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
        let lifelinker = create_creature(&mut state, PlayerId(0), "Vampire", 3, 3);
        state
            .objects
            .get_mut(&lifelinker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        setup_combat(&mut state, vec![prevented, lifelinker], Vec::new());
        if let Some(info) = state.combat.as_mut().and_then(|combat| {
            combat
                .attackers
                .iter_mut()
                .find(|a| a.object_id == lifelinker)
        }) {
            *info = AttackerInfo::attacking_player(lifelinker, PlayerId(2));
        }

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

        // The event records the assignment before state-based actions move Bear1.
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Object(target),
                amount: 5,
                is_combat: true,
                ..
            } if *source_id == attacker && *target == blocker1
        )));
        for blocker in [blocker1, blocker2] {
            assert!(events.iter().any(|event| matches!(
                event,
                GameEvent::DamageDealt {
                    source_id,
                    target: TargetRef::Object(target),
                    amount: 2,
                    is_combat: true,
                    ..
                } if *source_id == blocker && *target == attacker
            )));
        }
        assert_eq!(
            state.objects[&blocker1].zone,
            crate::types::zones::Zone::Graveyard,
            "CR 704.5g: the 2/2 blocker leaves after five combat damage"
        );
        assert_eq!(
            state.objects[&blocker2].zone,
            crate::types::zones::Zone::Battlefield,
            "the unassigned blocker remains on the battlefield"
        );
        assert_eq!(state.objects[&blocker2].damage_marked, 0);
        assert_eq!(
            state.objects[&attacker].zone,
            crate::types::zones::Zone::Battlefield,
            "Fatty survives with one less than lethal damage"
        );
        assert_eq!(state.objects[&attacker].damage_marked, 4);
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
        assert!(state.objects[&attacker]
            .keywords
            .contains(&Keyword::Deathtouch));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Object(target),
                amount: 1,
                is_combat: true,
                ..
            } if *source_id == attacker && *target == blocker
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Object(target),
                amount: 2,
                is_combat: true,
                ..
            } if *source_id == blocker && *target == attacker
        )));
        assert_eq!(
            state.objects[&blocker].zone,
            crate::types::zones::Zone::Graveyard,
            "CR 702.2b + CR 704.5h: the deathtouch-damaged Bear is destroyed"
        );
        assert_eq!(
            state.objects[&attacker].zone,
            crate::types::zones::Zone::Graveyard,
            "CR 704.5g: the 1/1 attacker is destroyed by the Bear's two damage"
        );
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
            attacked_permanents: std::iter::once((
                attacker,
                crate::types::identifiers::ObjectIncarnationRef::from_object(&state.objects[&pw]),
            ))
            .collect(),
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

    /// CR 510.1b + CR 506.4c + CR 702.19e: every automatic delegation
    /// uses the same ended relation, including the original-defender exception.
    #[test]
    fn ended_relation_automatic_and_as_though_unblocked_routes_agree() {
        for blocked in [false, true] {
            for over_pw in [false, true] {
                for ended in [false, true] {
                    let mut state = setup();
                    let attacker = create_creature(&mut state, PlayerId(0), "Attacker", 2, 2);
                    let pw = create_planeswalker(&mut state, PlayerId(1), "Walker", 10);
                    let target = AttackTarget::Planeswalker(pw);
                    let pin = crate::game::combat::bind_attack_target(&state, target).unwrap();
                    let mut info = AttackerInfo::new(attacker, target, PlayerId(1));
                    info.blocked = blocked;
                    if over_pw {
                        state
                            .objects
                            .get_mut(&attacker)
                            .unwrap()
                            .keywords
                            .push(Keyword::TrampleOverPlaneswalkers);
                    } else if blocked {
                        state
                            .objects
                            .get_mut(&attacker)
                            .unwrap()
                            .keywords
                            .push(Keyword::Trample);
                    }
                    state.combat = Some(CombatState {
                        attackers: vec![info.clone()],
                        attacked_permanents: std::iter::once((attacker, pin)).collect(),
                        ..Default::default()
                    });
                    assert_eq!(
                        info.resolve_damage_target(&state, over_pw),
                        Some(DamageTarget::Object(pw))
                    );
                    if ended {
                        crate::game::effects::remove_from_combat::remove_object_from_combat(
                            &mut state, pw,
                        );
                    }
                    let trample = if over_pw {
                        Some(TrampleKind::OverPlaneswalkers)
                    } else if blocked {
                        Some(TrampleKind::Standard)
                    } else {
                        None
                    };
                    let as_unblocked = assign_damage_as_though_unblocked(&state, &info, 2, trample);
                    assert_eq!(
                        as_unblocked,
                        if !ended {
                            vec![DamageAssignment {
                                target: DamageTarget::Object(pw),
                                amount: 2,
                            }]
                        } else if over_pw {
                            vec![DamageAssignment {
                                target: DamageTarget::Player(PlayerId(1)),
                                amount: 2,
                            }]
                        } else {
                            vec![]
                        }
                    );
                    let mut events = Vec::new();
                    assert!(resolve_combat_damage(&mut state, &mut events).is_none());
                    assert_eq!(state.objects[&pw].loyalty, Some(if ended { 10 } else { 8 }));
                    assert_eq!(
                        (state.players[0].life, state.players[1].life),
                        (20, if ended && over_pw { 18 } else { 20 })
                    );
                    let damage: Vec<_> = events
                        .iter()
                        .filter_map(|event| match event {
                            GameEvent::DamageDealt {
                                source_id,
                                target,
                                amount,
                                ..
                            } if *source_id == attacker => Some((target.clone(), *amount)),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(
                        damage,
                        if !ended {
                            vec![(TargetRef::Object(pw), 2)]
                        } else if over_pw {
                            vec![(TargetRef::Player(PlayerId(1)), 2)]
                        } else {
                            vec![]
                        }
                    );
                }
            }
        }
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
            attacked_permanents: std::iter::once((
                attacker,
                crate::types::identifiers::ObjectIncarnationRef::from_object(&state.objects[&pw]),
            ))
            .collect(),
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
            attacked_permanents: std::iter::once((
                attacker,
                crate::types::identifiers::ObjectIncarnationRef::from_object(&state.objects[&pw]),
            ))
            .collect(),
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
            attacked_permanents: std::iter::once((
                attacker,
                crate::types::identifiers::ObjectIncarnationRef::from_object(&state.objects[&pw]),
            ))
            .collect(),
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
            attacked_permanents: std::iter::once((
                attacker,
                crate::types::identifiers::ObjectIncarnationRef::from_object(&state.objects[&pw]),
            ))
            .collect(),
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
            attacked_permanents: std::iter::once((
                attacker,
                crate::types::identifiers::ObjectIncarnationRef::from_object(&state.objects[&pw]),
            ))
            .collect(),
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
            attacked_permanents: std::iter::once((
                attacker,
                crate::types::identifiers::ObjectIncarnationRef::from_object(&state.objects[&pw]),
            ))
            .collect(),
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
            attacked_permanents: std::iter::once((
                attacker,
                crate::types::identifiers::ObjectIncarnationRef::from_object(&state.objects[&pw]),
            ))
            .collect(),
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
        let prevented_count = |events: &[GameEvent]| {
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::DamagePrevented { .. }))
                .count()
        };

        // CR 510.4: the first combat-damage step is its own batch and its own
        // rider firing (4), ending with the CR 510.3 priority window.
        let first = resolve_combat_damage(&mut state, &mut events);
        assert!(matches!(first, Some(WaitingFor::Priority { .. })));
        assert_eq!(state.players[1].life, 20, "the first strike was prevented");
        assert_eq!(count_inklings(&state), 4, "first-strike step: 4 Inklings");
        assert_eq!(prevented_count(&events), 1, "one DamagePrevented so far");

        // The regular step is the second batch and the second firing (4).
        let second = resolve_combat_damage(&mut state, &mut events);
        assert!(second.is_none());

        assert_eq!(state.players[1].life, 20, "both strikes prevented");
        // CR 510.4: two separate combat-damage steps → two rider firings of 4.
        assert_eq!(
            count_inklings(&state),
            8,
            "double strike: 4 (first-strike step) + 4 (regular step) = 8"
        );
        // CR 510.4: two DamagePrevented events, one per combat-damage step.
        assert_eq!(
            prevented_count(&events),
            2,
            "one DamagePrevented per combat-damage step"
        );
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

    // ── First-striking blockers destroy the attacker only through damage they assign ──
    //
    // CR 702.7b + CR 704.5g + CR 704.5h + CR 702.12b: whether the attacker survives to the
    // regular step depends on the damage the first-striking blockers actually assign, not on
    // the assignment minimum a deathtouch source would need.

    /// The attacker already has lethal damage marked but is indestructible; the 0-power
    /// deathtouch blocker assigns nothing (CR 510.1a). First step: 2 to the blocker, 1 through;
    /// regular step: no blockers left, all 3 through (CR 702.19d).
    #[test]
    fn already_lethal_indestructible_double_striker_survives_a_zero_power_deathtouch_blocker() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 3, 3);
        kw(
            &mut state,
            a,
            vec![
                Keyword::DoubleStrike,
                Keyword::Trample,
                Keyword::Indestructible,
            ],
        );
        state.objects.get_mut(&a).unwrap().damage_marked = 3;
        let b = create_creature(&mut state, PlayerId(1), "B", 0, 2);
        kw(
            &mut state,
            b,
            vec![Keyword::FirstStrike, Keyword::Deathtouch],
        );
        assert_eq!(combat_damage_to_defender(&state, a, &[b]), 4);
    }

    /// CR 702.12b: deathtouch damage does not destroy an indestructible attacker, so it strikes
    /// again: 1 lethal and 2 through, then 3 through.
    #[test]
    fn indestructible_double_striker_survives_a_first_strike_deathtouch_blocker() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 3, 3);
        kw(
            &mut state,
            a,
            vec![
                Keyword::DoubleStrike,
                Keyword::Trample,
                Keyword::Indestructible,
            ],
        );
        let b = create_creature(&mut state, PlayerId(1), "B", 1, 1);
        kw(
            &mut state,
            b,
            vec![Keyword::FirstStrike, Keyword::Deathtouch],
        );
        assert_eq!(combat_damage_to_defender(&state, a, &[b]), 5);
    }

    /// CR 704.5g: a first-striking blocker whose damage is lethal still kills the attacker
    /// before the regular step (2 to the 2/3, nothing through; the 2/2 dies).
    #[test]
    fn lethal_first_strike_blocker_still_kills_a_double_striker() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 2, 2);
        kw(&mut state, a, vec![Keyword::DoubleStrike, Keyword::Trample]);
        let b = create_creature(&mut state, PlayerId(1), "B", 2, 3);
        kw(&mut state, b, vec![Keyword::FirstStrike]);
        assert_eq!(combat_damage_to_defender(&state, a, &[b]), 0);
    }

    /// CR 702.2b + CR 510.1a: deathtouch destroys only through damage its holder assigns. The
    /// deathtouch blocker has 0 power and the 1-power blocker has no deathtouch, so the 5/5
    /// survives the first step (2 lethal, 3 through) and tramples all 5 in the regular step.
    #[test]
    fn deathtouch_on_a_zero_power_first_striker_does_not_kill_the_attacker() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 5, 5);
        kw(&mut state, a, vec![Keyword::DoubleStrike, Keyword::Trample]);
        let b1 = create_creature(&mut state, PlayerId(1), "B1", 0, 1);
        kw(
            &mut state,
            b1,
            vec![Keyword::FirstStrike, Keyword::Deathtouch],
        );
        let b2 = create_creature(&mut state, PlayerId(1), "B2", 1, 1);
        kw(&mut state, b2, vec![Keyword::FirstStrike]);
        assert_eq!(combat_damage_to_defender(&state, a, &[b1, b2]), 8);
    }

    /// CR 702.2b + CR 704.5h: the control for the case above — deathtouch on the blocker that
    /// assigns 1 damage destroys the 5/5 after the first step's 3 through.
    #[test]
    fn deathtouch_on_a_damaging_first_striker_kills_the_attacker() {
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "A", 5, 5);
        kw(&mut state, a, vec![Keyword::DoubleStrike, Keyword::Trample]);
        let b1 = create_creature(&mut state, PlayerId(1), "B1", 0, 1);
        kw(&mut state, b1, vec![Keyword::FirstStrike]);
        let b2 = create_creature(&mut state, PlayerId(1), "B2", 1, 1);
        kw(
            &mut state,
            b2,
            vec![Keyword::FirstStrike, Keyword::Deathtouch],
        );
        assert_eq!(combat_damage_to_defender(&state, a, &[b1, b2]), 3);
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
    /// followed by a second combat damage step, and both are counted. The second
    /// step follows the CR 510.3 priority window after the first (one pass round
    /// later), not the first step in the same `resolve_combat_damage` call.
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

            // The first step is over and its priority window is open: its damage is
            // dealt (Youthful Knight 2, Fencing Ace 1) and only it has begun.
            let state = runner.state();
            assert!(
                matches!(state.waiting_for, WaitingFor::Priority { .. }),
                "double strike {double_strike}: {:?}",
                state.waiting_for
            );
            assert!(state.stack.is_empty());
            assert!(combat(&runner).first_strike_done && !combat(&runner).regular_damage_done);
            assert_eq!(
                runner.life(P1),
                life_before - if double_strike { 1 } else { 2 },
                "double strike {double_strike}: only the first step's damage is dealt"
            );
            assert_eq!(
                combat_damage_steps(&runner),
                1,
                "double strike {double_strike}: one combat damage step has begun"
            );

            // Both players pass the window; the second step begins.
            runner.pass_both_players();

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

    /// CR 510.3a + CR 510.4: a trigger from the first-strike step fills the
    /// (unconditional) priority window before the second step. The second step is
    /// counted once, when it begins through the `priority.rs` completeness gate,
    /// and not before.
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

        // The first step (the Knight's 2) ends in its priority window; the second
        // step has not begun.
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(combat(&runner).first_strike_done);
        assert!(!combat(&runner).regular_damage_done);
        assert!(combat(&runner).damage_step_index.is_none());
        assert_eq!(runner.life(P1), life_before - 2);
        assert_eq!(combat_damage_steps(&runner), 1);

        // Passing the window begins the second step, which prompts the Bears'
        // assignment.
        runner.pass_both_players();

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
        // #10392: riders live in the arena — install before the push borrows state.
        let rider_id = state.install_replacement_continuation(rider);
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::All)
                    .valid_card(TargetFilter::SelfRef)
                    .runtime_execute(rider_id)
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
            // #10392: riders live in the arena — install, then link the id.
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
            let rider_id = state.install_replacement_continuation(rider);
            def = def.runtime_execute(rider_id);
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

    /// The replacement pass of a combat-damage batch parked on a CR 616.1 choice.
    fn parked_replacement_pass(
        state: &GameState,
    ) -> Option<&crate::types::game_state::PendingCombatDamageReplacement> {
        state
            .pending_combat_lifelink
            .as_ref()
            .and_then(|record| record.replacement.as_ref())
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
        assert!(
            parked_replacement_pass(&state).is_some(),
            "the batch parks its replacement pass before applying anything"
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
        assert!(state.pending_combat_lifelink.is_none());
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
        assert!(state.pending_combat_lifelink.is_none());
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

        // A's answer completes A. CR 510.4: players receive priority before
        // the regular combat damage step; that step's batch B then parks.
        state.waiting_for = waiting_a.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "CR 510.4: priority between the first-strike and regular steps, got {:?}",
            state.waiting_for
        );
        let waiting_b = resolve_combat_damage(&mut state, &mut events).expect("batch B pauses");
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
        assert!(state.pending_combat_lifelink.is_none());
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
        assert!(
            parked_replacement_pass(&state).is_some(),
            "the overlap parks the batch's replacement pass"
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
        assert!(state.pending_combat_lifelink.is_none());
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
        // APNAP fixture: the blocker assigns no combat damage, so the batch
        // holds only E — the return strike would otherwise pause first under
        // APNAP order (its recipient is the active player's attacker, which
        // carries the same two-shield overlap N needs).
        let blocker = create_creature(&mut state, PlayerId(1), "Warded Bear", 0, 2);
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
        assert!(
            parked_replacement_pass(&state).is_some(),
            "the batch stays parked while N is open"
        );

        // Answer N: the nested damage delivers inline exactly once, and the
        // batch completes through the resume.
        state.waiting_for = waiting_n.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the batch settles after N, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_lifelink.is_none());
        assert!(state.combat.as_ref().unwrap().regular_damage_done);

        // (b) nested delivery: 5 reflected − 2 − 1 = 2, exactly once —
        // the only damage the attacker takes (the fixture blocker assigns
        // none, so there is no return leg).
        assert_eq!(
            state.objects[&attacker].damage_marked, 2,
            "reflection 2, delivered exactly once"
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
            vec![(2, false)],
            "the nested (noncombat) damage is delivered exactly once (the \
             fixture blocker assigns no return damage), got {events:?}"
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
        // (c) CR 700.1: the reflection is its own (noncombat) event, so N's
        // partial `Next(N)` preventions publish per event, naming the damage
        // source (the reflector's host), ahead of E's aggregate.
        assert!(
            prevented.contains(&(blocker, TargetRef::Object(attacker), 2))
                && prevented.contains(&(blocker, TargetRef::Object(attacker), 1)),
            "N's partial preventions publish per event, got {prevented:?}"
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
        // #10392: riders live in the arena — install before the push borrows state.
        let rider_id = state.install_replacement_continuation(rider);
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::Next(n))
                    .valid_card(TargetFilter::SelfRef)
                    .runtime_execute(rider_id)
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
        assert!(
            parked_replacement_pass(&state).is_some(),
            "the batch stays parked while N is open"
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
        assert!(state.pending_combat_lifelink.is_none());
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
        assert!(state.pending_combat_lifelink.is_none());
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
        assert!(state.pending_combat_lifelink.is_none());

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
        assert!(state.pending_combat_lifelink.is_none());

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
        assert!(state.pending_combat_lifelink.is_none());
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
        assert!(state.pending_combat_lifelink.is_none());
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
        assert!(state.pending_combat_lifelink.is_none());
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
        assert!(state.pending_combat_lifelink.is_none());
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
        assert!(state.pending_combat_lifelink.is_none());

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
        assert!(state.pending_combat_lifelink.is_none());
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

        // APNAP order: both P0 returns decide first, then E1 parks.
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
        assert!(
            parked_replacement_pass(&state).is_some(),
            "E1's overlap parks the batch's replacement pass"
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
        let fresh = state
            .pending_combat_lifelink
            .as_ref()
            .expect("the same batch re-parks on E2");
        assert!(fresh.replacement.is_some(), "E2 parks the replacement pass");
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
        assert!(state.pending_combat_lifelink.is_none());
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
        assert!(
            parked_replacement_pass(&state).is_some(),
            "the repeat keeps the batch parked"
        );

        // Second answer: the event settles and the one batch completes.
        state.waiting_for = waiting_second.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the batch settles after the repeat, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_lifelink.is_none());
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

    /// R3 with a non-empty Phase-B prefix: a singly-protected hit is an
    /// entry now (Matt R14-MED retired the Phase-A eager claim), decided
    /// inline during the Phase-B drain, while an overlap pauses in Phase B.
    /// The installs merge the parked Phase-B prefix with the answer's
    /// accumulation, so BOTH preventions publish once with correct
    /// per-(shield, recipient) amounts. The Phase-A leg is empty: the gate
    /// only proposes or skips, never tallies.
    ///
    /// REVERT PROBES: install only the answer leg → the singly-protected
    /// aggregate vanishes; publish per leg → one of them duplicates.
    #[test]
    fn f1_nonempty_phase_b_prefix_merges_across_pause() {
        use crate::types::keywords::{Keyword, ProtectionTarget};
        use crate::types::mana::ManaColor;

        let mut state = setup();
        // Singly-protected pair: an entry, decided inline during the drain.
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
        let record = state
            .pending_combat_lifelink
            .as_ref()
            .expect("the overlap parks the batch");
        assert!(
            record.replacement.is_some(),
            "the replacement pass is parked"
        );
        assert!(
            record
                .prevention_tally
                .iter()
                .any(|entry| entry.recipient == TargetRef::Object(guarded) && entry.amount == 3),
            "reach guard: the singly-protected hit is tallied in the parked prefix, got {:?}",
            record.prevention_tally
        );

        state.waiting_for = waiting.clone();
        answer_replacement_choice(&mut state, &mut events, 0);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the batch settles, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_combat_lifelink.is_none());
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
            "both returns (decided prefix) still land"
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
            parked_replacement_pass(&state).is_none(),
            "the replacement pass is consumed by its resume"
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
}
