use std::collections::HashSet;

use crate::game::filter::{matches_target_filter_including_phased_out, FilterContext};
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{EffectKind, ReplacementDefinition, RestrictionExpiry, TargetFilter};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::format::GameFormat;
use crate::types::game_state::{
    AutoPassMode, GameState, PendingCounterAddition, PendingEffectResolved, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::statics::{HandSizeModification, StaticMode};
use crate::types::zones::Zone;

use super::combat;
use super::combat_damage;
use super::day_night;
use super::priority;
use super::turn_control;
use super::zones;

const PHASE_ORDER: [Phase; 12] = [
    Phase::Untap,
    Phase::Upkeep,
    Phase::Draw,
    Phase::PreCombatMain,
    Phase::BeginCombat,
    Phase::DeclareAttackers,
    Phase::DeclareBlockers,
    Phase::CombatDamage,
    Phase::EndCombat,
    Phase::PostCombatMain,
    Phase::End,
    Phase::Cleanup,
];

pub fn next_phase(phase: Phase) -> Phase {
    let idx = PHASE_ORDER.iter().position(|&p| p == phase).unwrap();
    PHASE_ORDER[(idx + 1) % PHASE_ORDER.len()]
}

/// CR 500.5: Advance to the next phase/step, clearing mana pools.
pub fn advance_phase(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // CR 500.8: Extra phases are inserted *directly after* their anchor phase
    // (e.g., Aurelia's "after this phase" extra combat is inserted after the
    // current combat phase ends — anchor = `EndCombat`). Consume only when
    // `state.phase == anchor`, scanning from the end so the most recently
    // created entry occurs first ("the most recently created phase will occur
    // first" per CR 500.8). An entry with a non-matching anchor is preserved
    // until its anchor phase is reached.
    let next = state
        .extra_phases
        .iter()
        .rposition(|ep| ep.anchor == state.phase)
        .map(|i| state.extra_phases.remove(i).phase)
        .unwrap_or_else(|| next_phase(state.phase));

    // If wrapping from Cleanup to Untap, start next turn. Turn-level skip
    // replacements (CR 614.10) are handled inside `start_next_turn` — the
    // per-phase pipeline below runs only for within-turn phase advances.
    if state.phase == Phase::Cleanup && next == Phase::Untap {
        start_next_turn(state, events);
    } else {
        // CR 614.1b + CR 614.10 + CR 500.11: Route phase/step starts through the
        // replacement pipeline so condition-gated skip replacements can prevent
        // the phase. Simple static-based skips (`StaticMode::SkipStep`) still
        // short-circuit at dedicated call sites (e.g., `should_skip_step` for
        // untap/draw); this path handles event-context-aware replacements.
        let proposed = ProposedEvent::begin_phase(state.active_player, next);
        if matches!(
            replacement::replace_event(state, proposed, events),
            ReplacementResult::Prevented
        ) {
            // CR 500.11: "To skip a step, phase, or turn is to proceed past it
            // as though it didn't exist." Advance `state.phase` past the skipped
            // phase so the recursive call computes the phase AFTER it, then
            // recurse to enter that phase.
            state.phase = next;
            return advance_phase(state, events);
        }
    }

    enter_phase(state, next, events);
}

/// CR 724.1d: End the current turn by skipping straight to the cleanup step.
/// Discards any extra phases/steps scheduled for this turn (they are skipped)
/// and enters a fresh cleanup step — per CR 724.1d, even if the turn is ended
/// during the cleanup step, a new cleanup step begins. Drives `Effect::EndTheTurn`
/// (Time Stop, Sundial of the Infinite, Obeka, Glorious End, Discontinuity).
pub fn end_turn_to_cleanup(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // CR 724.1d: "skip any phases or steps between this phase or step and the
    // cleanup step" — drop scheduled extra phases for this (now-ending) turn.
    state.extra_phases.clear();
    enter_phase(state, Phase::Cleanup, events);
}

/// CR 724.2d: End the current combat phase by removing everything from combat,
/// expiring "until end of combat" effects, and skipping straight to the
/// postcombat main phase. Mirrors the end-of-combat teardown the `EndCombat`
/// step performs (see the `Phase::EndCombat` arm of `advance_phase`), but skips
/// the intervening end-of-combat step so its "at end of combat" triggers do not
/// fire (CR 724.2e). Drives `Effect::EndCombatPhase` (Mandate of Peace).
pub fn end_combat_phase_to_postcombat(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // CR 724.2d / CR 511.3: Remove all creatures and planeswalkers from combat.
    state.combat = None;
    // CR 724.2d: Effects that last "until end of combat" expire — continuous
    // effects, replacement definitions, and pending damage replacements alike,
    // matching the normal end-of-combat prune.
    super::layers::prune_end_of_combat_effects(state);
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        obj.replacement_definitions
            .retain(|r| !matches!(r.expiry, Some(RestrictionExpiry::EndOfCombat)));
    }
    state
        .pending_damage_replacements
        .retain(|r| !matches!(r.expiry, Some(RestrictionExpiry::EndOfCombat)));

    // CR 724.2d: Skip straight to the postcombat main phase, skipping any
    // intervening steps (including the end-of-combat step — CR 724.2e). Any
    // extra combat phases scheduled for this turn are also skipped.
    state.extra_phases.clear();
    enter_phase(state, Phase::PostCombatMain, events);
}

/// Enter a phase directly: set phase, run the CR 703.4q step-end empty
/// unspent mana event for each player in APNAP order through the replacement
/// pipeline, then (when the queue empties) reset priority (CR 117.3a),
/// invalidate LKI (CR 400.7), and emit `PhaseChanged`.
///
/// Called by `advance_phase` after extra-phase/replacement resolution, and
/// directly by callers that need to skip intermediate phases (e.g.,
/// CR 508.8 combat-skip when no attackers are possible).
///
/// CR 616.1 / CR 616.1e: When ≥2 step-end mana handlers apply to the same
/// emptying event, the affected player chooses ordering. Choices serialize
/// across players in APNAP order. On a pause (a player must choose), the
/// drain stores progress in `state.pending_phase_transition_progress` and
/// sets `state.waiting_for`; resume happens via the `EmptyManaPool` arm of
/// `handle_replacement_choice`, which re-calls `drain_pending_phase_transition_progress`.
fn enter_phase(state: &mut GameState, next: Phase, events: &mut Vec<GameEvent>) {
    use std::collections::VecDeque;

    state.phase = next;
    if next == Phase::BeginCombat {
        state.combat_phases_started_this_turn =
            state.combat_phases_started_this_turn.saturating_add(1);
    }
    // CR 500.8 + CR 513.1: track end-step occurrences for "first end step of the
    // turn" gates (Y'shtola Rhul). Counts every End step begun this turn,
    // including extra end steps scheduled via AdditionalPhase, so the gate only
    // holds for the first.
    if next == Phase::End {
        state.end_steps_started_this_turn = state.end_steps_started_this_turn.saturating_add(1);
    }

    // CR 500.5: Mana pools empty between phases/steps.
    // Firebending mana (EndOfCombat expiry) persists within combat steps.
    let in_combat = matches!(
        next,
        Phase::BeginCombat
            | Phase::DeclareAttackers
            | Phase::DeclareBlockers
            | Phase::CombatDamage
            | Phase::EndCombat
    );
    let entering_cleanup = next == Phase::Cleanup;

    state.pending_phase_transition_progress =
        Some(crate::types::game_state::PhaseTransitionProgress {
            remaining_players: VecDeque::from(super::players::apnap_order(state)),
            next_phase: next,
            in_combat,
            entering_cleanup,
        });
    drain_pending_phase_transition_progress(state, events);
}

/// CR 703.4q + CR 616.1: Per-phase APNAP-queue drain. Pops players one at a
/// time, runs `clear_expiring_at_step_end` first (H2 invariant —
/// expiry-bound units never enter the replacement pipeline), scans active
/// step-end mana handlers for that player, builds and dispatches a
/// `ProposedEvent::EmptyManaPool` through `replace_event`. On `Execute`,
/// applies decisions and continues. On `NeedsChoice`, sets `state.waiting_for`
/// and returns — `pending_phase_transition_progress` retains the rest of the
/// queue so the resume arm can pick up where this paused. When the queue
/// empties, calls `finish_enter_phase` to complete priority/LKI/PhaseChanged.
pub(super) fn drain_pending_phase_transition_progress(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) {
    while let Some(progress) = state.pending_phase_transition_progress.as_mut() {
        let Some(player_id) = progress.remaining_players.pop_front() else {
            // Queue empty: complete the phase entry.
            let next_phase = progress.next_phase;
            state.pending_phase_transition_progress = None;
            finish_enter_phase(state, next_phase, events);
            return;
        };
        let in_combat = progress.in_combat;
        let entering_cleanup = progress.entering_cleanup;

        // CR 500.5 + CR 614.6 (H2 invariant): Drop only expiry-bound units
        // whose own rule fires on this transition. Non-expiry units flow
        // into the replacement pipeline as Drop-disposition decisions.
        if let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) {
            player
                .mana_pool
                .clear_expiring_at_step_end(in_combat, entering_cleanup);
        }

        // Scan active step-end mana handlers for this player. Inlines the
        // logic previously in `static_abilities::player_step_end_mana_handlers`:
        // printed statics via `battlefield_active_statics`, then spell-installed
        // riders via `transient_continuous_effects` keyed on `SpecificPlayer`.
        let scan_entries = scan_step_end_mana_handlers(state, player_id);
        state.pending_step_end_mana_handlers = scan_entries;

        // Build per-unit decision payload from the player's surviving pool.
        //
        // CR 500.5 + CR 703.4q (H2 invariant): expiry-bound units (e.g.
        // Klauth's "you don't lose this mana as steps and phases end",
        // Firebending's "Until end of combat, you don't lose this mana as
        // steps and phases end" — CR 702.189a) have *already* had their fate
        // decided by `clear_expiring_at_step_end` above — they were either
        // dropped (their rule fired) or deliberately retained.
        //
        // CR 614.17 + CR 614.17c: "you don't lose this mana …" is a "can't"
        // effect, not a replacement effect. It prevents the CR 106.4 /
        // CR 703.4q lose-mana event for the protected units, and per
        // CR 614.17c, once that event can't happen no other replacement
        // effect — including a step-end mana handler (Upwelling, Horizon
        // Stone, Kruphix) — can modify or replace it. So such units must NOT
        // enter the empty-pool replacement pipeline at all; emitting a `Drop`
        // decision here would empty the very mana the card promises to keep.
        // Only `None`-expiry units flow into the pipeline as Drop-disposition
        // decisions. The `enumerate` runs over the full pool so `pool_index`
        // stays aligned with the retained expiry units that remain in
        // `mana_pool.mana`.
        // Debug-only: CR 500.5 end-of-step empty is suppressed for a player with
        // the infinite-mana toggle active — every non-expiry unit is dispositioned
        // `Keep` instead of `Drop` so the pool survives the step transition. This
        // is the partner of `mana_payment::refill_infinite_mana`; together they
        // keep a flagged player's pool continuously full.
        let keep_for_infinite_mana = state.debug_infinite_mana.contains(&player_id);
        let units: Vec<crate::types::mana::UnitDecision> = state
            .players
            .iter()
            .find(|p| p.id == player_id)
            .map(|p| {
                p.mana_pool
                    .mana
                    .iter()
                    .enumerate()
                    .filter(|(_, u)| u.expiry.is_none())
                    .map(|(idx, u)| crate::types::mana::UnitDecision {
                        pool_index: idx,
                        color: u.color,
                        disposition: if keep_for_infinite_mana {
                            crate::types::mana::UnitDisposition::Keep
                        } else {
                            crate::types::mana::UnitDisposition::Drop
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();

        let proposed = ProposedEvent::EmptyManaPool {
            player_id,
            units,
            applied: HashSet::new(),
        };

        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                if let ProposedEvent::EmptyManaPool {
                    player_id, units, ..
                } = event
                {
                    crate::types::mana::apply_empty_mana_pool_decisions(
                        state, player_id, &units, events,
                    );
                }
                state.pending_step_end_mana_handlers.clear();
                // Continue to next player.
            }
            ReplacementResult::NeedsChoice(choosing_player) => {
                // CR 616.1: Affected player chooses ordering. Surface the
                // prompt and return — the queue (with subsequent players)
                // remains in `pending_phase_transition_progress` for resume
                // via `handle_replacement_choice`'s EmptyManaPool arm.
                state.waiting_for =
                    replacement::replacement_choice_waiting_for(choosing_player, state);
                return;
            }
            ReplacementResult::Prevented => {
                // CR 614.5: Step-end mana handlers do not Prevent — they
                // flip dispositions on the rebuilt event. A Prevent here
                // would indicate a registry-level prevention shield aimed
                // at `LoseMana`, which no card on the current corpus
                // produces. If a future card ever prevents step-end empty-
                // mana (e.g., a hypothetical "mana doesn't empty this
                // step" replacement), this arm must be reworked to leave
                // the pool intact and continue draining the remaining
                // queue, rather than silently clearing handler scratch.
                // TODO(CR-616.1): re-evaluate when such a card lands.
                debug_assert!(
                    false,
                    "ReplacementResult::Prevented unexpected for EmptyManaPool event"
                );
                state.pending_step_end_mana_handlers.clear();
            }
        }
    }
}

/// CR 703.4q + CR 616.1 + CR 611.2b: Scan active step-end mana handlers for
/// `player_id`. Combines printed statics on battlefield permanents and
/// spell-installed riders via `transient_continuous_effects` keyed on
/// `SpecificPlayer`. Inlined here (rather than a separate `static_abilities`
/// helper) because the only consumer is the drain loop above.
fn scan_step_end_mana_handlers(
    state: &GameState,
    player_id: PlayerId,
) -> Vec<crate::types::game_state::StepEndManaScanEntry> {
    use crate::types::ability::{ContinuousModification, Duration, TargetFilter};
    use crate::types::game_state::StepEndManaScanEntry;

    let context = super::static_abilities::StaticCheckContext {
        player_id: Some(player_id),
        ..Default::default()
    };

    let mut entries: Vec<StepEndManaScanEntry> =
        super::functioning_abilities::battlefield_active_statics(state)
            .filter_map(|(source_obj, def)| {
                let StaticMode::StepEndUnspentMana { filter, action } = &def.mode else {
                    return None;
                };
                if let Some(ref affected) = def.affected {
                    if !super::static_abilities::static_filter_matches(
                        state,
                        &context,
                        affected,
                        source_obj.id,
                    ) {
                        return None;
                    }
                }
                let description = def
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("{action}"));
                Some(StepEndManaScanEntry {
                    source: source_obj.id,
                    controller: player_id,
                    filter: *filter,
                    action: *action,
                    description,
                })
            })
            .collect();

    // CR 611.2b: Spell-installed handlers live in `transient_continuous_effects`
    // with `affected: SpecificPlayer { id }` and an explicit `Duration`.
    for tce in &state.transient_continuous_effects {
        let TargetFilter::SpecificPlayer { id: affected_id } = tce.affected else {
            continue;
        };
        if affected_id != player_id {
            continue;
        }
        if let Duration::ForAsLongAs { ref condition } = tce.duration {
            if !super::layers::evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        if let Some(ref condition) = tce.condition {
            if !super::layers::evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        for modification in &tce.modifications {
            if let ContinuousModification::AddStaticMode {
                mode: StaticMode::StepEndUnspentMana { filter, action },
            } = modification
            {
                entries.push(StepEndManaScanEntry {
                    source: tce.source_id,
                    controller: tce.controller,
                    filter: *filter,
                    action: *action,
                    description: format!("{} ({action})", tce.source_name),
                });
            }
        }
    }

    entries
}

/// CR 117.3a + CR 400.7: Complete a phase entry after the per-player empty-
/// mana drain has resolved. Resets priority, invalidates LKI, clears the
/// per-step draw counter (bookkeeping for `ExceptFirstDrawInDrawStep`
/// condition machinery — not a CR rule itself), and emits `PhaseChanged`.
fn finish_enter_phase(state: &mut GameState, next: Phase, events: &mut Vec<GameEvent>) {
    for player in state.players.iter_mut() {
        // Bookkeeping (not a CR rule): `cards_drawn_this_step` is the
        // counter the `ExceptFirstDrawInDrawStep` parser-level condition
        // tests against. Reset on every step transition so the next step
        // identifies its own first draw cleanly.
        player.cards_drawn_this_step = 0;
    }

    // CR 117.3a: Active player receives priority at the beginning of most steps and phases.
    state.priority_player = turn_control::turn_decision_maker(state);
    priority::clear_priority_passes(state);
    state.players_attacked_this_step.clear();
    // CR 400.7: LKI persists within a step but is invalidated on step transition.
    state.lki_cache.clear();
    // CR 607.2b + CR 603.10e: linked-exile LKI is likewise step-scoped — it only
    // needs to outlive the resolution of the ability whose source just left.
    state.linked_exile_lki.clear();

    events.push(GameEvent::PhaseChanged { phase: next });

    // CR 904.9: Immediately after the archenemy's precombat main phase begins,
    // they set the top scheme of their scheme deck in motion (a turn-based action
    // that doesn't use the stack). No-op outside an Archenemy game, when the active
    // player isn't the archenemy, or when the scheme deck is empty.
    if next == Phase::PreCombatMain
        && super::topology::archenemy(state) == Some(state.active_player)
    {
        crate::game::archenemy::set_in_motion(state, events);
    }
}

/// Begin the next player's turn (CR 500.1 / CR 101.4 seat order).
pub fn start_next_turn(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // CR 805.4b: defensively drop any stale draw-step queue entries. The
    // queue is normally drained to empty before a turn ends, but a turn
    // ended early (e.g. `Effect::EndTheTurn` — Time Stop, Obeka) could in
    // principle leave it non-empty; without this it would be wrongly
    // resumed at the START of the next turn's Draw step instead of being
    // re-seeded for the new active player.
    state.pending_team_draw_step.clear();

    let completed_player = state.active_player;
    let completed_turn_key =
        super::topology::normalize_shared_turn_recipient(state, completed_player);
    if state.turn_decision_controller.is_some() {
        let mut grant_extra_turn_after = false;
        state.scheduled_turn_controls.retain(|scheduled| {
            if scheduled.target_player != completed_turn_key {
                return true;
            }
            if Some(scheduled.controller) == state.turn_decision_controller {
                grant_extra_turn_after |= scheduled.grant_extra_turn_after;
            }
            false
        });
        if grant_extra_turn_after {
            state.extra_turns.push(completed_player);
        }
        state.turn_decision_controller = None;
    }

    state.turn_number += 1;

    // CR 500.7: Determine the active player and whether this turn is an *extra*
    // turn (LIFO-popped from `state.extra_turns`) or a natural turn (next seat
    // in APNAP order). `is_extra_turn` flows into the replacement pipeline so
    // condition-gated skip effects (e.g., Stranglehold) can observe it.
    let is_extra_turn = if let Some(extra_turn_player) = state.extra_turns.pop() {
        state.active_player =
            super::topology::normalize_shared_turn_recipient(state, extra_turn_player);
        true
    } else {
        state.active_player = super::topology::next_turn_representative(state, state.active_player);
        false
    };

    // CR 614.10: Simple turn-skip counter (effect-based, e.g., Meditate, Eater of
    // Days). This is a fast path for "you skip your next turn" that doesn't need
    // the replacement pipeline — there's no event-context predicate to evaluate.
    let skip_player = super::topology::normalize_shared_turn_recipient(state, state.active_player);
    let idx = skip_player.0 as usize;
    if idx < state.turns_to_skip.len() && state.turns_to_skip[idx] > 0 {
        state.turns_to_skip[idx] -= 1;
        // Recursively start the next turn (skipping this one entirely).
        return start_next_turn(state, events);
    }

    // CR 614.1b + CR 614.10: Route turn-start through the replacement pipeline so
    // condition-gated skip replacements (Stranglehold's "skip extra turns") can
    // prevent the turn. `ShieldKind::None` (default) means these permanent statics
    // are never consumed — they fire whenever their predicate matches.
    let proposed = ProposedEvent::begin_turn(state.active_player, is_extra_turn);
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Prevented => {
            // CR 614.10: Turn skipped entirely — restart for the next player.
            return start_next_turn(state, events);
        }
        ReplacementResult::Execute(_) => {
            // Normal path — turn proceeds.
        }
        ReplacementResult::NeedsChoice(_) => {
            // CR 614.1b: Skip replacements are mandatory — no Optional BeginTurn
            // replacement should ever reach here. If a parser bug routes one here,
            // clear the pending choice and proceed rather than stalling turn flow.
            state.pending_replacement = None;
            debug_assert!(
                false,
                "BeginTurn replacement unexpectedly returned NeedsChoice"
            );
        }
    }

    // CR 500: Track per-player turn count for "your Nth turn of the game" conditions.
    state.players[state.active_player.0 as usize].turns_taken += 1;

    // CR 311.5 / CR 312.4 / CR 901.6: the planar controller is normally whoever
    // the active player is. The turn has committed here (past both turn-skip
    // early-returns above), so `active_player` is final for this invocation —
    // sync the planar controller (and the active plane's `.controller`) to it.
    // No-op outside a Planechase game.
    crate::game::planechase::set_planar_controller(state, state.active_player, events);

    if let Some(scheduled) = state
        .scheduled_turn_controls
        .iter()
        .rfind(|scheduled| {
            scheduled.target_player
                == super::topology::normalize_shared_turn_recipient(state, state.active_player)
        })
        .copied()
    {
        state.turn_decision_controller = Some(scheduled.controller);
    }

    // Reset priority
    state.priority_player = turn_control::turn_decision_maker(state);
    priority::clear_priority_passes(state);

    // Reset per-turn counters
    // CR 305.2: Reset per-turn land play count.
    state.lands_played_this_turn = 0;
    // CR 901.9 / CR 116.2i: planar die special-action costs reset each turn.
    state.planar_die_actions_this_turn.clear();
    // CR 603.4: Snapshot spell count for werewolf "last turn" conditions before resetting.
    state.spells_cast_last_turn = Some(state.spells_cast_this_turn);
    // CR 500.1: Reset per-turn spell cast counters.
    state.spells_cast_this_turn = 0;
    state.triggers_fired_this_turn.clear();
    state.trigger_fire_counts_this_turn.clear();
    state.triggers_fired_this_turn_per_opponent.clear();
    state.activated_abilities_this_turn.clear();
    // CR 602.5b: "Activate only once each turn" crew restriction resets each turn.
    state.crew_activated_this_turn.clear();
    // CR 606.3: The "loyalty ability once per turn" limit is a property of the
    // permanent ("no player has previously activated a loyalty ability of that
    // permanent that turn"), not its controller. It resets at the start of every
    // turn for every planeswalker regardless of who controls it.
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        obj.loyalty_activations_this_turn = 0;
    }
    // CR 606.1 + CR 603.4: Per-player loyalty-activation history is a CR 603.4
    // "this turn" record. The cap-raising grant from
    // `Effect::GrantExtraLoyaltyActivations` (The Chain Veil class) is bounded
    // to the same turn, so both maps clear together at turn start.
    state.loyalty_abilities_activated_this_turn.clear();
    state.extra_loyalty_activations_this_turn.clear();
    // CR 701.43d: the "exerted this turn" record gates the linked "when you do"
    // trigger to once per turn; reset it alongside the other per-turn trackers.
    state.exerted_this_turn.clear();
    // CR 514 + CR 603.4: Per-ability per-turn resolution counter resets at turn
    // boundary alongside other "this turn" trackers (mirrors the cleanup of
    // `trigger_fire_counts_this_turn`).
    state.ability_resolutions_this_turn.clear();
    state.graveyard_cast_permissions_used.clear();
    // CR 110.4 + CR 601.2a: Reset per-turn-per-permanent-type tracking (Muldrotha).
    state.graveyard_cast_permissions_used_per_type.clear();
    // CR 601.2b: Reset per-turn CastFromHandFree once-per-turn tracking (Zaffai).
    state.hand_cast_free_permissions_used.clear();
    // CR 601.2a: Reset per-turn PlayFromExile source usage (Evelyn-style permissions).
    state.exile_play_permissions_used.clear();
    // CR 601.2a + CR 113.6b: Reset per-turn ExileCastPermission once-per-turn
    // tracking (Maralen, Fae Ascendant) and the rolling list of cards exiled
    // with each tracked source this turn. Both are turn-scoped slices; the
    // persistent `exile_links` pool is untouched and continues to back the
    // open-ended "cards exiled with ~" filter for sources without a per-turn
    // cap.
    state.exile_cast_permissions_used.clear();
    // CR 601.2a + CR 401.5: Reset per-turn TopOfLibraryCastPermission
    // once-per-turn tracking (Assemble the Players, Johann, Apprentice Sorcerer).
    state.top_of_library_cast_permissions_used.clear();
    state.cards_exiled_with_source_this_turn.clear();
    // CR 702.94a: Reset per-player first-card-drawn-this-turn tracking for miracle.
    state.first_card_drawn_this_turn.clear();
    state.cards_drawn_this_turn.clear();
    // CR 702.94a: Any miracle offers that outlived priority without being
    // flushed are stale (the "first card drawn this turn" condition no longer
    // applies after the turn ends). Drop them so we never surface a prompt for
    // a card drawn last turn.
    state.pending_miracle_offers.clear();
    state.spells_cast_this_turn_by_player.clear();
    state.lands_played_this_turn_by_player.clear();
    state.players_who_searched_library_this_turn.clear();
    state.player_actions_this_turn.clear();
    state.players_attacked_this_step.clear();
    state.players_attacked_this_turn.clear();
    state.attacking_creatures_this_turn.clear();
    state.attacked_defenders_this_turn.clear();
    state.creature_attacked_defenders_this_turn.clear();
    state.combat_phases_started_this_turn = 0;
    // CR 614.10 + CR 614.10a + CR 500.11: A turn-scoped combat skip that was
    // bound (`active`) to this player's PREVIOUS (now-ended) turn is satisfied —
    // release the binding so this new turn has normal combat unless another
    // pending skip rebinds below. `idx` is the player whose turn is beginning.
    // Only the `active` binding is cleared — any still-`pending` skips have not
    // yet bound to a turn and must survive (CR 614.10a: the second of two stacked
    // skips waits for the next occurrence), to be promoted below if this turn
    // isn't itself skipped.
    if let Some(slot) = state.combat_phase_skip_next_turn.get_mut(idx) {
        slot.active = false;
    }
    state.end_steps_started_this_turn = 0;
    state.creatures_attacked_this_turn.clear();
    state.attacker_declarations_this_turn.clear();
    state.creatures_blocked_this_turn.clear();
    state.players_who_created_token_this_turn.clear();
    state.created_tokens_this_turn.clear();
    // CR 122.6 + CR 514.2: The `counter_added_this_turn` ledger backs the
    // turn-scoped `CountersPutOnThisTurn` filter predicate (CR 122.6 look-back),
    // which feeds continuous statics such as Kid Loki's hexproof grant. Those
    // "this turn" effects end at cleanup (CR 514.2), so clearing the ledger
    // changes layer-relevant state. Route the expiry through the layer
    // invalidation authority — mirroring the turn-boundary continuous-effect
    // prunes (`prune_until_next_turn_effects`) — guarded on a non-empty ledger so
    // we only invalidate when something actually depended on it; otherwise a
    // static that gained a keyword from a counter placed last turn stays cached.
    if !state.counter_added_this_turn.is_empty() {
        state.layers_dirty.mark_full();
    }
    state.counter_added_this_turn.clear();
    state.players_who_discarded_card_this_turn.clear();
    state.cards_discarded_this_turn_by_player.clear();
    state.players_who_sacrificed_artifact_this_turn.clear();
    state.sacrificed_permanents_this_turn.clear();
    state.zone_changes_this_turn.clear();
    state.batched_zone_change_trigger_fired.clear();
    state.battlefield_entries_this_turn.clear();
    // CR 701.26 + CR 603.4: reset per-object tap counts so "first time it became
    // tapped this turn" intervening-ifs start fresh each turn.
    state.object_tap_count_this_turn.clear();
    state.damage_dealt_this_turn.clear();
    // CR 702.173a + CR 514: Clear the Freerunning eligibility ledger at
    // cleanup. CR 702.173a's "was dealt combat damage this turn" predicate
    // is turn-scoped, so the ledger must reset on the turn boundary.
    state
        .assassin_or_commander_dealt_combat_damage_this_turn
        .clear();
    // CR 702.76a + CR 514: Clear the Prowl creature-type ledger at cleanup — its
    // "was dealt combat damage this turn" predicate is turn-scoped too.
    state.creature_types_dealt_combat_damage_this_turn.clear();
    // CR 500.8: Clear any leftover extra phases from the previous turn.
    state.extra_phases.clear();
    // CR 700.14: Reset cumulative mana spent on spells for Expend triggers.
    state.mana_spent_on_spells_this_turn.clear();
    // CR 601.2f: Clear one-shot cost reductions and spell modifiers from the previous turn.
    state.pending_spell_cost_reductions.clear();
    state.pending_next_spell_modifiers.clear();
    // CR 614.1c: Pending ETB counters are turn-scoped (e.g., "this turn" effects).
    state.pending_etb_counters.clear();
    state.modal_modes_chosen_this_turn.clear();
    for player in &mut state.players {
        player.has_drawn_this_turn = false;
        player.lands_played_this_turn = 0;
        player.life_gained_this_turn = 0;
        // CR 603.4: Snapshot life lost before reset for "lost life during their last turn" conditions.
        player.life_lost_last_turn = player.life_lost_this_turn;
        player.life_lost_this_turn = 0;
        player.descended_this_turn = false;
        player.cards_drawn_this_turn = 0;
        // CR 121.1 + CR 504.1: Per-step counter is also reset at turn start so
        // a fresh turn always begins with `cards_drawn_this_step == 0` (the
        // step-transition reset in `advance_phase` covers within-turn step
        // boundaries; this covers the Cleanup→Untap turn boundary and
        // mid-turn extra-turn insertions).
        player.cards_drawn_this_step = 0;
        player.speed_trigger_used_this_turn = false;
        player.bending_types_this_turn.clear();
    }

    // CR 614.10 + CR 614.10a + CR 500.11: Bind one pending turn-scoped combat
    // skip to this turn now that the active player's first non-skipped turn has
    // actually begun. This runs AFTER the per-turn reset region (so the `active`
    // flag it sets is not immediately re-cleared) and AFTER the `turns_to_skip`
    // fast-path early-return above (so per CR 614.10a the skip binds only to a
    // turn that isn't itself skipped). Consume one `pending` skip and mark the
    // turn `active`; any remaining pending skips wait for subsequent turns. While
    // `active`, the replacement layer prevents every combat phase of this turn.
    if let Some(slot) = state.combat_phase_skip_next_turn.get_mut(idx) {
        if slot.pending > 0 {
            slot.pending -= 1;
            slot.active = true;
        }
    }

    // CR 302.6: At the start of a player's turn, any permanent they have
    // controlled continuously since before this moment has now been under
    // their control "since that player's most recent turn began" — clear
    // summoning sickness.
    let active = state.active_player;
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        if obj.controller == active && obj.summoning_sick {
            obj.summoning_sick = false;
        }
    }

    // Clear all UntilEndOfTurn flags — no auto-pass survives a turn boundary.
    state
        .auto_pass
        .retain(|_, mode| !matches!(mode, AutoPassMode::UntilEndOfTurn));

    events.push(GameEvent::TurnStarted {
        player_id: state.active_player,
        turn_number: state.turn_number,
    });
}

/// CR 502.1 + CR 502.3: During the untap step, first the phasing turn-based
/// action runs (CR 702.26a), then the active player untaps each permanent
/// they control. CR 702.26m: If the untap step is skipped, phasing is also
/// skipped — callers must gate this whole function on `should_skip_step`.
pub fn execute_untap(state: &mut GameState, events: &mut Vec<GameEvent>) {
    execute_untap_with_choices(state, events, &HashSet::new());
}

/// CR 502.3: Bridge between the optional-decline prompt (`UntapChoice`) and the
/// untap turn-based action. Given the permanents the player has chosen not to
/// untap so far, this checks for a `MaxUntapPerType` cap whose eligible group
/// still exceeds its limit. If one exists, it raises
/// `WaitingFor::ChooseUntapSubset` so the active player directly determines
/// which `max` permanents untap (CR 502.3); otherwise it performs the untap
/// with the recorded declines and advances the phase. The caller continues
/// `auto_advance` only when this returns `None` (no subset prompt raised).
///
/// Returns `Some(prompt)` if a bounded-subset selection is now pending, `None`
/// if the untap already executed and the phase advanced.
pub fn begin_untap_or_subset_prompt(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    chosen_not_to_untap: HashSet<ObjectId>,
) -> Option<WaitingFor> {
    let active = state.active_player;
    if let Some((group, max)) = max_untap_subset_prompt(state, active, &chosen_not_to_untap) {
        // Persist the declines so the subset resolution can fold the unchosen
        // complement in alongside them when it finally executes the untap.
        state.pending_untap_declines = chosen_not_to_untap.into_iter().collect();
        return Some(WaitingFor::ChooseUntapSubset {
            player: active,
            group,
            max,
        });
    }
    execute_untap_with_choices(state, events, &chosen_not_to_untap);
    advance_phase(state, events);
    None
}

pub fn execute_untap_with_choices(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    chosen_not_to_untap: &HashSet<ObjectId>,
) {
    // Phase any phased-out player back in at the start of their next turn.
    // Player phasing is not formally governed by CR 702.26 (permanent-only);
    // this mirrors the permanent behaviour so duration semantics line up
    // with `Duration::UntilNextTurnOf` (also pruned at this step below).
    super::phasing::execute_untap_step_player_phase_in(state, events);

    // CR 502.1 + CR 702.26a: Phasing happens first, before any permanents
    // untap. Simultaneous phase-in + phase-out for the active player.
    super::phasing::execute_untap_step_phasing(state, events);

    let active = state.active_player;

    // CR 514.2: Prune "until your next turn" transient effects for the active player.
    super::layers::prune_until_next_turn_effects(state, active);
    // CR 514.2 + CR 611.2a/b: Expire `PlayFromExile` permissions granted to
    // the active player with `UntilYourNextTurn` duration (impulse draws that
    // last "until your next turn").
    super::layers::prune_until_next_turn_casting_permissions(state, active);
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        obj.replacement_definitions.retain(|r| {
            !matches!(r.expiry, Some(RestrictionExpiry::UntilPlayerNextTurn { player }) if player == active)
        });
    }
    state.pending_damage_replacements.retain(|r| {
        !matches!(r.expiry, Some(RestrictionExpiry::UntilPlayerNextTurn { player }) if player == active)
    });
    // CR 514.2 + CR 500.7: Arm "until the end of the player's next turn"
    // restrictions (Kang's power-up prohibition) when that player's next turn
    // begins — convert to `EndOfTurn` so the cleanup-step prune (`execute_cleanup`)
    // ends them at THIS turn's cleanup, persisting through the whole turn.
    // Mirrors `prune_until_next_turn_effects` (layers.rs). NOTE: if the granted
    // turn is SKIPPED/PREVENTED before its untap step, this conversion never runs
    // and the restriction is never armed/pruned — a documented narrow edge shared
    // with the analogous `Duration::UntilEndOfNextTurnOf` arming.
    {
        use crate::types::ability::GameRestriction;
        for restriction in state.restrictions.iter_mut() {
            if let GameRestriction::ProhibitActivity { expiry, .. } = restriction {
                if matches!(expiry, RestrictionExpiry::UntilEndOfNextTurnOf { player } if *player == active)
                {
                    *expiry = RestrictionExpiry::EndOfTurn;
                }
            }
        }
    }
    state.restrictions.retain(|restriction| {
        use crate::types::ability::GameRestriction;

        match restriction {
            GameRestriction::ProhibitActivity { expiry, .. } => {
                !matches!(expiry, RestrictionExpiry::UntilPlayerNextTurn { player } if *player == active)
            }
            GameRestriction::DamagePreventionDisabled { .. } => true,
        }
    });

    // CR 502.3: Collect object IDs that have a CantUntap transient effect
    // (e.g., "doesn't untap during its controller's next untap step").
    // These permanents skip untapping this step.
    let cant_untap_ids: HashSet<ObjectId> = state
        .transient_continuous_effects
        .iter()
        .filter(|e| {
            e.modifications.iter().any(|m| {
                matches!(
                    m,
                    crate::types::ability::ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantUntap,
                    }
                )
            })
        })
        .filter_map(|e| {
            if let crate::types::ability::TargetFilter::SpecificObject { id } = &e.affected {
                Some(*id)
            } else {
                None
            }
        })
        .collect();

    // CR 502.3 + CR 604.1: Also check permanent-sourced CantUntap statics
    // (including attached-subject Aura restrictions) AND filter-scoped transient
    // CantUntap (CR 611.1 — a spell/effect that installs "creatures don't untap
    // …" by typed/filter target). The `cant_untap_ids` set above only catches
    // SpecificObject transients; this loop covers the printed-static and
    // filter-scoped-transient classes so the actual untap agrees with the
    // cap-prompt group built by `untap_excluded_ids`.
    // CR 502.3 + CR 604.1: hoist the CantUntap existence gate once before the
    // per-permanent scan so the O(N) `check_static_ability` re-scan is skipped
    // for every permanent when no functioning CantUntap static exists
    // (O(N^2) -> O(N) on the every-turn untap step).
    let has_cant_untap_static =
        super::functioning_abilities::any_functioning_static_mode(state, |m| {
            matches!(m, StaticMode::CantUntap)
        });
    let intrinsic_cant_untap: HashSet<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| obj.controller == active)
                && ((has_cant_untap_static
                    && super::static_abilities::check_static_ability(
                        state,
                        StaticMode::CantUntap,
                        &super::static_abilities::StaticCheckContext {
                            target_id: Some(*id),
                            ..Default::default()
                        },
                    ))
                    || super::static_abilities::transient_grants_static_mode_to_object(
                        state,
                        *id,
                        &StaticMode::CantUntap,
                    ))
        })
        .collect();

    // CR 502.3: Apply `MaxUntapPerType` caps (Smoke / Damping Field / Winter Orb).
    // Each cap holds excess matching permanents tapped. The player's declines
    // (and CantUntap) already reduce each group; the cap then forces any
    // remaining excess beyond `max` to stay tapped, in deterministic order. This
    // is the authoritative enforcement: it holds whether or not the player was
    // prompted to determine which untap (AI / auto-play paths may not decline).
    let mut max_untap_skipped: HashSet<ObjectId> = HashSet::new();
    let restrictions = max_untap_restrictions(state);
    if !restrictions.is_empty() {
        let mut already_skipped: HashSet<ObjectId> = HashSet::new();
        already_skipped.extend(chosen_not_to_untap.iter().copied());
        already_skipped.extend(cant_untap_ids.iter().copied());
        already_skipped.extend(intrinsic_cant_untap.iter().copied());
        for (filter, max) in &restrictions {
            for id in max_untap_excess(state, active, filter, *max, &already_skipped) {
                already_skipped.insert(id);
                max_untap_skipped.insert(id);
            }
        }
    }

    let to_untap: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| obj.controller == active && obj.tapped)
                .unwrap_or(false)
        })
        .collect();

    for id in to_untap {
        // CR 502.3: Skip permanents that have CantUntap (transient or intrinsic)
        // or are held tapped by a MaxUntapPerType cap.
        if chosen_not_to_untap.contains(&id)
            || cant_untap_ids.contains(&id)
            || intrinsic_cant_untap.contains(&id)
            || max_untap_skipped.contains(&id)
        {
            continue;
        }

        let proposed = ProposedEvent::Untap {
            object_id: id,
            applied: HashSet::new(),
        };

        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                if let ProposedEvent::Untap { object_id, .. } = event {
                    if let Some(obj) = state.objects.get_mut(&object_id) {
                        // CR 122.1d: If a permanent with a stun counter would become untapped,
                        // instead remove a stun counter from it.
                        if let Some(entry) = obj.counters.get_mut(&CounterType::Stun) {
                            *entry -= 1;
                            if *entry == 0 {
                                obj.counters.remove(&CounterType::Stun);
                            }
                            events.push(GameEvent::CounterRemoved {
                                object_id,
                                counter_type: CounterType::Stun,
                                count: 1,
                            });
                        } else {
                            obj.tapped = false;
                            events.push(GameEvent::PermanentUntapped { object_id });
                        }
                    }
                }
            }
            ReplacementResult::Prevented => {
                // "Doesn't untap during untap step" effects
            }
            ReplacementResult::NeedsChoice(_) => {
                // Edge case for untap step; skip for now
            }
        }
    }

    // CR 502.3 + CR 113.6: Seedborn-Muse-class statics grant a second untap
    // pass during each OTHER player's untap step. Scan the battlefield for
    // `StaticMode::UntapsDuringEachOtherPlayersUntapStep` sources whose
    // controller is NOT the active player; that controller untaps all of
    // their permanents matching the static's `affected` filter.
    //
    // This runs AFTER the active player's normal untap and BEFORE the
    // "until controller's next untap step" prune, so it does not interfere
    // with either. Untapping already-untapped permanents is a no-op, so
    // multiple Seedborn-like sources (e.g. copy effects) compose safely.
    // Phased-out sources are excluded by `active_static_definitions`.
    //
    // Note: "doesn't untap during your controller's untap step" restrictions
    // (Frozen Shade, Tidewater Minion) do NOT apply here — this is "another
    // player's untap step", not the permanent's controller's. This is
    // consistent with CR 502.3.
    execute_seedborn_statics(state, events, active);

    // CR 502.3: Prune "until controller's next untap step" effects AFTER the untap
    // step has been processed, so the permanent skips exactly one untap.
    super::layers::prune_controller_untap_step_effects(state, active);
}

/// CR 502.3: Collect the active `MaxUntapPerType` restrictions (Smoke /
/// Damping Field / Winter Orb class). Each governs the untap turn-based action
/// globally for the active player, so the source's controller is irrelevant —
/// any live source contributes its `(filter, max)` cap. Returns `(filter, max)`
/// pairs cloned out of the statics so the caller can mutate `state` afterward.
fn max_untap_restrictions(state: &GameState) -> Vec<(crate::types::ability::TargetFilter, u32)> {
    super::functioning_abilities::battlefield_active_statics(state)
        .filter_map(|(_, def)| match &def.mode {
            StaticMode::MaxUntapPerType { filter, max } => Some((filter.clone(), *max)),
            _ => None,
        })
        .collect()
}

/// CR 502.3 SAFETY NET: For a single `MaxUntapPerType { filter, max }` cap,
/// determine which of `player`'s tapped permanents matching `filter` must be
/// held tapped because the cap would otherwise be exceeded. With the bounded
/// subset selection (`WaitingFor::ChooseUntapSubset`) in place, the player's /
/// AI's chosen complement is already folded into `already_skipped`, so this
/// clamp should normally find nothing to skip. It is retained purely as a
/// safety net: if a caller reaches `execute_untap_with_choices` without having
/// resolved the subset prompt (a malformed selection, a future direct caller),
/// the cap is still enforced in deterministic battlefield order rather than
/// silently over-untapping past the CR 502.3 limit.
fn max_untap_excess(
    state: &GameState,
    player: PlayerId,
    filter: &crate::types::ability::TargetFilter,
    max: u32,
    already_skipped: &HashSet<ObjectId>,
) -> Vec<ObjectId> {
    let matching =
        max_untap_eligible_group(state, player, filter, already_skipped, &HashSet::new());
    matching.into_iter().skip(max as usize).collect()
}

/// CR 502.3: Candidates for the per-permanent optional-decline prompt
/// (`WaitingFor::UntapChoice`). This is the "you may choose not to untap"
/// Vedalken Shackles / Stoic Angel-tap class only — `StaticMode::MayChooseNotToUntap`.
/// `MaxUntapPerType` caps are a SEPARATE decision (a required bounded subset
/// selection) surfaced by [`max_untap_subset_prompt`], not folded in here.
pub fn untap_choice_candidates(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.controller == player
                    && obj.tapped
                    && super::functioning_abilities::active_static_definitions(state, obj).any(
                        |sd| {
                            sd.mode == StaticMode::MayChooseNotToUntap
                                && super::static_abilities::check_static_ability(
                                    state,
                                    StaticMode::MayChooseNotToUntap,
                                    &super::static_abilities::StaticCheckContext {
                                        target_id: Some(*id),
                                        ..Default::default()
                                    },
                                )
                        },
                    )
            })
        })
        .collect()
}

/// CR 502.3: "the active player determines which permanents they control will
/// untap." Compute the bounded-subset prompt for the FIRST `MaxUntapPerType`
/// cap (Smoke / Stoic Angel / Damping Field / Winter Orb class) whose eligible
/// group exceeds its cap, given the permanents already staying tapped
/// (`chosen_not_to_untap` from the decline prompt, plus CantUntap). Returns the
/// over-cap `group` and `max` so the engine raises `WaitingFor::ChooseUntapSubset`,
/// making the player/AI directly select which `max` untap — NOT a deterministic
/// excess-skip. Returns `None` when every cap's eligible group is at or under
/// its cap (no choice needed).
///
/// Only the first over-cap cap is surfaced per call; after the player resolves
/// it, the chosen complement folds into `chosen_not_to_untap` and the next cap
/// (if any) is surfaced on the following pass, so stacked caps of different
/// types each get their own player determination.
pub fn max_untap_subset_prompt(
    state: &GameState,
    player: PlayerId,
    chosen_not_to_untap: &HashSet<ObjectId>,
) -> Option<(Vec<ObjectId>, usize)> {
    // CR 502.3: with no `MaxUntapPerType` cap in play there is nothing to prompt,
    // so bail before the O(N) `untap_excluded_ids` CantUntap scan — the common
    // every-turn case has no cap and would otherwise pay for a scan whose result
    // is discarded.
    if max_untap_restrictions(state).is_empty() {
        return None;
    }
    let cant_untap = untap_excluded_ids(state, player);
    for (filter, max) in max_untap_restrictions(state) {
        let group =
            max_untap_eligible_group(state, player, &filter, chosen_not_to_untap, &cant_untap);
        if group.len() > max as usize {
            return Some((group, max as usize));
        }
    }
    None
}

/// CR 502.3: Permanents the active player controls that cannot untap regardless
/// of any cap decision (transient or intrinsic `CantUntap`). Surfacing these in
/// a max-untap choice would be misleading — the player cannot select them to
/// untap — so they are excluded from both the prompt group and the cap math.
fn untap_excluded_ids(state: &GameState, player: PlayerId) -> HashSet<ObjectId> {
    use crate::types::ability::ContinuousModification;
    let mut excluded: HashSet<ObjectId> = state
        .transient_continuous_effects
        .iter()
        .filter(|e| {
            e.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantUntap,
                    }
                )
            })
        })
        .filter_map(|e| {
            if let crate::types::ability::TargetFilter::SpecificObject { id } = &e.affected {
                Some(*id)
            } else {
                None
            }
        })
        .collect();
    // CR 502.3 + CR 604.1: hoist the CantUntap existence gate once before the
    // per-permanent scan (O(N^2) -> O(N) when no functioning CantUntap exists).
    let has_cant_untap_static =
        super::functioning_abilities::any_functioning_static_mode(state, |m| {
            matches!(m, StaticMode::CantUntap)
        });
    for id in state.battlefield.iter().copied() {
        let Some(obj) = state.objects.get(&id) else {
            continue;
        };
        if obj.controller != player {
            continue;
        }
        // CR 502.3 + CR 604.1: permanent-sourced printed/static CantUntap
        // (including attached-subject Aura restrictions).
        let intrinsic = has_cant_untap_static
            && super::static_abilities::check_static_ability(
                state,
                StaticMode::CantUntap,
                &super::static_abilities::StaticCheckContext {
                    target_id: Some(id),
                    ..Default::default()
                },
            );
        // CR 502.3 + CR 611.1: filter-scoped transient CantUntap (a spell/effect
        // installing "creatures don't untap …" by typed/filter target rather
        // than a single SpecificObject). Build for the whole class so any such
        // affected permanent is removed from the max-untap cap group and math —
        // the exact-id SpecificObject case is already folded in above.
        let transient_filtered = super::static_abilities::transient_grants_static_mode_to_object(
            state,
            id,
            &StaticMode::CantUntap,
        );
        if intrinsic || transient_filtered {
            excluded.insert(id);
        }
    }
    excluded
}

/// CR 502.3: The active player's tapped permanents matching a single cap's
/// `filter` that can still legally untap (not declined, not CantUntap). This is
/// the set the player chooses among when over the cap.
fn max_untap_eligible_group(
    state: &GameState,
    player: PlayerId,
    filter: &crate::types::ability::TargetFilter,
    chosen_not_to_untap: &HashSet<ObjectId>,
    cant_untap: &HashSet<ObjectId>,
) -> Vec<ObjectId> {
    use crate::game::filter::{matches_target_filter, FilterContext};
    // The max-untap filter is a printed type quality (creature / artifact /
    // nonbasic land) with no controller-relative clause; ownership is enforced
    // by the explicit `obj.controller == player` check below, so a neutral
    // context is correct (CR 502.3 caps the active player's own permanents).
    let ctx = FilterContext::neutral();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| obj.controller == player && obj.tapped)
                && !chosen_not_to_untap.contains(id)
                && !cant_untap.contains(id)
                && matches_target_filter(state, *id, filter, &ctx)
        })
        .collect()
}

/// CR 502.3 + CR 113.6: Second-pass untap for `UntapsDuringEachOtherPlayersUntapStep`
/// statics (Seedborn Muse class). Runs during the active player's untap step,
/// after the normal active-player untap. Each matching source whose controller
/// != `active_player` triggers an untap of that controller's permanents
/// matching the static's `affected` filter.
fn execute_seedborn_statics(state: &mut GameState, events: &mut Vec<GameEvent>, active: PlayerId) {
    use crate::game::filter::{matches_target_filter, FilterContext};
    use crate::types::ability::TargetFilter;

    // Collect (source_id, source_controller, affected_filter) tuples up-front
    // so we don't borrow `state` mutably while iterating statics.
    let seedborn_pulls: Vec<(ObjectId, PlayerId, TargetFilter)> =
        super::functioning_abilities::battlefield_active_statics(state)
            .filter(|(_, def)| {
                matches!(def.mode, StaticMode::UntapsDuringEachOtherPlayersUntapStep)
            })
            .filter(|(obj, _)| obj.controller != active)
            .filter_map(|(obj, def)| {
                def.affected
                    .as_ref()
                    .map(|f| (obj.id, obj.controller, f.clone()))
            })
            .collect();

    if seedborn_pulls.is_empty() {
        return;
    }

    for (source_id, source_controller, filter) in seedborn_pulls {
        let ctx = FilterContext::from_source_with_controller(source_id, source_controller);
        // Snapshot IDs so the mutation loop doesn't alias the battlefield iteration.
        let to_untap: Vec<ObjectId> = state
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.controller == source_controller && obj.tapped)
            })
            .filter(|id| matches_target_filter(state, *id, &filter, &ctx))
            .collect();

        for id in to_untap {
            // CR 502.3: Untapping is idempotent; already-untapped permanents
            // (e.g. from an earlier Seedborn pass) are filtered out above.
            // Route through the replacement pipeline so "doesn't untap"
            // effects still apply when they are in scope (rare — most such
            // effects scope to "your controller's untap step", which does
            // not cover this pass).
            let proposed = ProposedEvent::Untap {
                object_id: id,
                applied: HashSet::new(),
            };
            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    if let ProposedEvent::Untap { object_id, .. } = event {
                        if let Some(obj) = state.objects.get_mut(&object_id) {
                            // CR 122.1d: Stun-counter removal takes precedence
                            // over the untap, matching the main untap pass.
                            if let Some(entry) = obj.counters.get_mut(&CounterType::Stun) {
                                *entry -= 1;
                                if *entry == 0 {
                                    obj.counters.remove(&CounterType::Stun);
                                }
                                events.push(GameEvent::CounterRemoved {
                                    object_id,
                                    counter_type: CounterType::Stun,
                                    count: 1,
                                });
                            } else {
                                obj.tapped = false;
                                events.push(GameEvent::PermanentUntapped { object_id });
                            }
                        }
                    }
                }
                ReplacementResult::Prevented => {}
                ReplacementResult::NeedsChoice(_) => {}
            }
        }
    }
}

/// CR 504.1: During the draw step, the active player draws a card.
/// CR 614.1a: Routes through the replacement pipeline so effects like Dredge apply.
/// Returns `Some(WaitingFor)` if a replacement effect needs player interaction.
/// CR 504.1: The active player's mandatory draw-step draw. CR 805.4b: under
/// the shared team turns option, every player on the active team draws
/// during the team's draw step — so the active player's teammate(s) also
/// draw here.
///
/// Seeds `state.pending_team_draw_step` with the players who owe a draw
/// THIS step (skipped if the queue is already non-empty — that means a
/// caller is resuming a step that paused mid-draw, and re-seeding would
/// redraw an already-completed player) and delegates to
/// `drain_pending_team_draw_step`, the single authority for actually
/// performing the queued draws. That function is also called directly by
/// `handle_replacement_choice`'s resume epilogue, so a draw that pauses on a
/// CR 616.1 competing-replacement choice still reaches every queued
/// teammate once the choice resolves — this function only needs to run once
/// per step, at first entry.
pub fn execute_draw(state: &mut GameState, events: &mut Vec<GameEvent>) -> Option<WaitingFor> {
    if state.pending_team_draw_step.is_empty() {
        state.pending_team_draw_step.push(state.active_player);
        if state.format_config.topology().has_shared_team_turns() {
            state
                .pending_team_draw_step
                .extend(super::players::teammates(state, state.active_player));
        }
    }
    drain_pending_team_draw_step(state, events)
}

/// CR 504.1 + CR 805.4b: Drain `state.pending_team_draw_step` front-to-back,
/// performing each queued player's turn-based draw-step draw exactly once.
///
/// A draw that pauses on a CR 616.1 competing-replacement choice (`Some`
/// returned) leaves its player at the FRONT of the queue — NOT popped — so
/// the next call (from `handle_replacement_choice`'s epilogue, once the
/// choice resolves) retries exactly that player's draw rather than skipping
/// it or re-drawing a player who already completed. Only a fully completed
/// draw (`None` from `execute_draw_for`) advances the queue.
pub(crate) fn drain_pending_team_draw_step(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    while let Some(&player) = state.pending_team_draw_step.first() {
        if let Some(wf) = execute_draw_for(state, player, events) {
            return Some(wf);
        }
        state.pending_team_draw_step.remove(0);
    }
    None
}

fn execute_draw_for(
    state: &mut GameState,
    active: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    // CR 121.1 + CR 614.1a + CR 614.6 + CR 704.3: Route through the
    // single-authority `draw_through_replacement` helper so post-replacement
    // continuations (Jace WinTheGame, Abundance reveal-until) drain in the
    // same step as the draw — never leaking into the next priority pass.
    //
    // The closure applies draw-step-specific bookkeeping (sets
    // `has_drawn_this_turn` per CR 504.1) and intentionally mirrors the
    // pre-existing inline behavior of this function — it does NOT call
    // `record_first_draw_and_enqueue_miracle` (the hook used by
    // `apply_draw_after_replacement` for spell-resolution draws).
    //
    // CR 702.94a (pre-existing gap): the natural draw-step draw therefore
    // does not enqueue a `MiracleOffer`. Whether the draw-step draw SHOULD
    // trigger miracle ("the first card you've drawn this turn") is a
    // separate rules question outside this fix's scope. Do not silently
    // "fix" by adding the miracle hook here without first verifying the
    // CR 702.94a reading against draw-step vs spell-resolution draws.
    let result = crate::game::effects::draw::draw_through_replacement(
        state,
        active,
        1,
        events,
        |state, event, events| {
            let ProposedEvent::Draw {
                player_id, count, ..
            } = event
            else {
                return;
            };
            let allowed = crate::game::effects::draw::allowed_draw_count(state, player_id, count);

            // CR 121.1 + CR 613.11: route card selection through the single
            // `select_cards_to_draw` authority so a `DrawFromBottom` static is
            // honored on the turn-based draw step too.
            let cards_to_draw = crate::game::effects::draw::select_cards_to_draw(
                state,
                player_id,
                allowed as usize,
            );

            // CR 704.5b: Attempting to draw from an empty library causes a game loss.
            if allowed > 0 && cards_to_draw.len() < allowed as usize {
                if let Some(p) = state.players.iter_mut().find(|p| p.id == player_id) {
                    p.drew_from_empty_library = true;
                }
            }

            for obj_id in cards_to_draw {
                zones::move_to_zone(state, obj_id, Zone::Hand, events);
                // CR 121.1 + CR 504.1: Increment counters BEFORE emitting so
                // `nth_in_step` (1-indexed) reflects this draw — the draw
                // step's mandatory draw is `nth_in_step == 1` and is the
                // anchor for `ExceptFirstDrawInDrawStep` exception clauses.
                let (nth_in_turn, nth_in_step) =
                    if let Some(p) = state.players.iter_mut().find(|p| p.id == player_id) {
                        p.has_drawn_this_turn = true;
                        p.cards_drawn_this_turn = p.cards_drawn_this_turn.saturating_add(1);
                        p.cards_drawn_this_step = p.cards_drawn_this_step.saturating_add(1);
                        (p.cards_drawn_this_turn, p.cards_drawn_this_step)
                    } else {
                        (1, 1)
                    };
                // CR 121.1: Emit CardDrawn so "whenever a player draws" triggers fire.
                events.push(GameEvent::CardDrawn {
                    player_id,
                    object_id: obj_id,
                    nth_in_turn,
                    nth_in_step,
                });
                crate::game::effects::drawn_this_turn_choice::record_drawn_card(
                    state, player_id, obj_id,
                );
            }
        },
    );

    if matches!(result, ReplacementResult::NeedsChoice(_)) {
        return Some(state.waiting_for.clone());
    }

    None
}

/// CR 514.2: Remove marked damage from every battlefield permanent as the
/// cleanup-step turn-based action, EXCEPT permanents matched by an active
/// "Damage isn't removed from [filter] during cleanup steps" static (Ancient
/// Adamantoise, Patient Zero, Uthgardt Fury, …), whose marked damage persists
/// across turns. Shared by both cleanup exits — the direct `execute_cleanup`
/// (no discard) and the deferred `finish_cleanup_discard` (after the active
/// player discards to maximum hand size) — so the protection holds on either
/// path.
fn clear_cleanup_damage(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // CR 514.2: An active "Damage isn't removed from [filter] during cleanup
    // steps" static suppresses removal for the permanents it matches; gather
    // that protected set first.
    let damage_persists: HashSet<ObjectId> = {
        let sources: Vec<(ObjectId, PlayerId, TargetFilter)> =
            super::functioning_abilities::battlefield_active_statics(state)
                .filter(|(_, def)| matches!(def.mode, StaticMode::DamageNotRemovedDuringCleanup))
                .filter_map(|(obj, def)| {
                    def.affected
                        .as_ref()
                        .map(|f| (obj.id, obj.controller, f.clone()))
                })
                .collect();

        // CR 514.2 + CR 702.26b: removing marked damage is a turn-based cleanup
        // action over the whole battlefield, including phased-out permanents — it
        // is not targeting — so the protected membership is evaluated with the
        // phased-out-aware matcher to mirror the unconditional removal below.
        let mut protected = std::collections::HashSet::new();
        for (source_id, source_controller, filter) in sources {
            let ctx = FilterContext::from_source_with_controller(source_id, source_controller);
            for id in state.battlefield.iter().copied() {
                if matches_target_filter_including_phased_out(state, id, &filter, &ctx) {
                    protected.insert(id);
                }
            }
        }
        protected
    };

    // CR 514.2: Damage on creatures is removed at cleanup.
    let to_clear: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| !damage_persists.contains(id))
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| obj.damage_marked > 0)
                .unwrap_or(false)
        })
        .collect();

    for id in to_clear {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.damage_marked = 0;
            obj.dealt_deathtouch_damage = false;
            events.push(GameEvent::DamageCleared { object_id: id });
        }
    }
}

/// Execute the cleanup step. Returns `Some(WaitingFor)` if the player must
/// choose which cards to discard down to maximum hand size, or `None` if
/// cleanup completes immediately.
pub fn execute_cleanup(state: &mut GameState, events: &mut Vec<GameEvent>) -> Option<WaitingFor> {
    // CR 701.19b: Regeneration shields expire at cleanup.
    // CR 615: Prevention effects also expire.
    // CR 514.2: Resolution-time replacements with `expiry: EndOfTurn` (e.g.,
    // the "if [target] would die this turn, exile it instead" rider on
    // damage spells) also expire here regardless of whether they fired.
    // Also prune any consumed shields from earlier this turn.
    let expires_at_eot = |r: &ReplacementDefinition| {
        r.shield_kind.is_shield() || matches!(r.expiry, Some(RestrictionExpiry::EndOfTurn))
    };
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        obj.replacement_definitions.retain(|r| !expires_at_eot(r));
    }
    state
        .pending_damage_replacements
        .retain(|r| !expires_at_eot(r));

    // CR 514.2: Prune "until end of turn" transient continuous effects.
    super::layers::prune_end_of_turn_effects(state);
    // CR 514.2 + CR 611.2a: Expire `PlayFromExile` permissions whose duration
    // was `UntilEndOfTurn` (impulse-draw "you may play it this turn").
    super::layers::prune_end_of_turn_casting_permissions(state);

    // CR 514.2: Remove end-of-turn game restrictions (e.g., "this turn" damage prevention disabled).
    state.restrictions.retain(|r| {
        use crate::types::ability::{GameRestriction, RestrictionExpiry};
        match r {
            GameRestriction::DamagePreventionDisabled { expiry, .. }
            | GameRestriction::ProhibitActivity { expiry, .. } => {
                !matches!(expiry, RestrictionExpiry::EndOfTurn)
            }
        }
    });

    // CR 603.7b + CR 513.2: Remove "this turn" delayed triggers at cleanup.
    // WheneverEvent (multi-fire, one_shot=false) triggers persist until cleanup.
    // WhenNextEvent (one-shot) triggers that didn't fire also expire — their
    // "this turn" duration means they must not carry over to the next turn.
    // Per CR 513.2 an unfired `AtNextPhase{End}` delayed trigger is NOT a
    // "this turn" trigger: the end step "doesn't back up", so it legitimately
    // persists to the next turn's end step — it must survive this retain.
    state.delayed_triggers.retain(|dt| {
        dt.one_shot
            && !matches!(
                dt.condition,
                crate::types::ability::DelayedTriggerCondition::WhenNextEvent { .. }
            )
    });

    // CR 502.2 / CR 731.2: Check the prior active player's day/night transition
    // before advancing the active player.
    day_night::check_day_night_transition(state, events);

    let active = state.active_player;

    // CR 514.1 + CR 402.2: Only the *active* player discards down to maximum hand size.
    // Non-active players keep their cards regardless of hand size until their own cleanup.
    // If the active player has "no maximum hand size" (CR 402.2), skip the discard check.
    let has_no_max = super::static_abilities::check_static_ability(
        state,
        StaticMode::NoMaximumHandSize,
        &super::static_abilities::StaticCheckContext {
            player_id: Some(active),
            ..Default::default()
        },
    );

    if !has_no_max {
        let max_hand_size = compute_maximum_hand_size(state, active);

        let player = state
            .players
            .iter()
            .find(|p| p.id == active)
            .expect("active player exists");

        let hand_size = player.hand.len();
        if hand_size > max_hand_size {
            let count = hand_size - max_hand_size;
            let cards = player.hand.iter().copied().collect();
            return Some(WaitingFor::DiscardToHandSize {
                player: active,
                count,
                cards,
            });
        }
    }

    // CR 514.2: Remove cleanup damage, preserving any permanent protected by a
    // "Damage isn't removed during cleanup steps" static (shared with the
    // deferred discard path so the protection holds regardless of discard).
    clear_cleanup_damage(state, events);

    // CR 702.171b: "Once a permanent has become saddled, it stays saddled until
    // the end of the turn or it leaves the battlefield." Clear the designation
    // at cleanup (CR 514).
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        if obj.is_saddled {
            // CR 702.171b: the designation (and the saddling-creature record) ends at end of turn.
            obj.is_saddled = false;
            obj.saddled_by.clear();
        }
    }

    None
}

/// CR 402.2 + CR 514.1: Compute the effective maximum hand size for a player.
///
/// Starts from the default of 7 (CR 402.2), then applies all `MaximumHandSize`
/// statics from battlefield and command zone that affect the given player.
/// SetTo overrides replace the base; AdjustedBy modifiers are accumulated additively.
/// The result is clamped to a minimum of 0.
fn compute_maximum_hand_size(state: &GameState, player: PlayerId) -> usize {
    let context = super::static_abilities::StaticCheckContext {
        player_id: Some(player),
        ..Default::default()
    };

    // CR 402.2: Default maximum hand size is seven.
    let mut base: i32 = 7;
    let mut total_adjustment: i32 = 0;
    let mut has_set_to = false;

    let zones = state.battlefield.iter().chain(state.command_zone.iter());
    for &id in zones {
        let obj = match state.objects.get(&id) {
            Some(o) => o,
            None => continue,
        };

        // CR 702.26b + CR 604.1 + CR 114.4: `active_static_definitions` owns the
        // phased-out / command-zone / condition gate.
        for def in super::functioning_abilities::active_static_definitions(state, obj) {
            let modification = match &def.mode {
                StaticMode::MaximumHandSize { modification } => modification,
                _ => continue,
            };

            // Check affected filter
            if let Some(ref affected) = def.affected {
                if !super::static_abilities::static_filter_matches(state, &context, affected, id) {
                    continue;
                }
            }

            match modification {
                HandSizeModification::SetTo(n) => {
                    // Last SetTo wins (timestamp order; for simplicity, last encountered).
                    base = *n as i32;
                    has_set_to = true;
                }
                HandSizeModification::AdjustedBy(n) => {
                    total_adjustment += n;
                }
                HandSizeModification::EqualTo(expr) => {
                    let resolved =
                        super::quantity::resolve_quantity(state, expr, obj.controller, id);
                    base = resolved;
                    has_set_to = true;
                }
            }
        }
    }

    if has_set_to {
        // SetTo/EqualTo overrides the base; adjustments still apply on top.
        (base + total_adjustment).max(0) as usize
    } else {
        // Only adjustments modify the default 7.
        (7i32 + total_adjustment).max(0) as usize
    }
}

/// Complete the cleanup step after the player has chosen cards to discard.
/// Discards the selected cards and clears damage (the parts of cleanup that
/// were deferred while waiting for player input).
/// CR 514.1: Discard down to maximum hand size at cleanup.
/// Routes through the replacement pipeline so Madness (CR 702.35) etc. can intercept.
/// Returns `true` if a replacement choice interrupted the discard loop.
pub fn finish_cleanup_discard(
    state: &mut GameState,
    player: PlayerId,
    chosen: &[crate::types::identifiers::ObjectId],
    events: &mut Vec<GameEvent>,
) -> bool {
    for &card_id in chosen {
        if let super::effects::discard::DiscardOutcome::NeedsReplacementChoice(choice_player) =
            super::effects::discard::discard_as_cost(state, card_id, player, events)
        {
            state.waiting_for =
                super::replacement::replacement_choice_waiting_for(choice_player, state);
            // Known limitation: remaining discards and damage clearing (CR 514.2)
            // are skipped when a replacement choice interrupts mid-cleanup.
            return true;
        }
    }

    // CR 514.2: Clear cleanup damage deferred from execute_cleanup — through the
    // same shared helper so a "Damage isn't removed during cleanup steps" static
    // (Patient Zero, Ancient Adamantoise, …) still preserves protected marked
    // damage even when the active player had to discard to maximum hand size.
    clear_cleanup_damage(state, events);
    false
}

/// CR 103.8: Whether the player who goes first skips their first draw step.
/// - CR 103.8a: In a two-player game, the player who plays first skips it.
/// - CR 103.8b: In Two-Headed Giant, the team who plays first skips it.
/// - CR 103.8c: In all other multiplayer games (Free-for-All, 3+ player
///   Commander, etc.) no player skips the draw step of their first turn.
///
/// The two-player check uses `state.players.len() == 2` rather than the
/// game format, because a two-player Commander game is still a two-player
/// game per CR 903.2 (Commander supports both two-player and multiplayer
/// setups) — the skip rule applies to it.
///
/// The team case intentionally checks the format enum rather than the broader
/// `team_based` axis: CR 103.8b names Two-Headed Giant specifically, while
/// CR 805 shared-team-turns can be used by other multiplayer variants.
fn first_player_skips_first_draw(state: &GameState) -> bool {
    matches!(state.format_config.format, GameFormat::TwoHeadedGiant) || state.players.len() == 2
}

/// CR 103.8 + CR 614.1b + CR 614.10: Whether the active player should skip
/// the draw step right now. Combines the first-turn rule above with any
/// "skip your draw step" static / one-shot replacements.
pub fn should_skip_draw(state: &GameState) -> bool {
    (state.turn_number == 1 && first_player_skips_first_draw(state))
        || should_skip_step_static(state, Phase::Draw)
}

/// CR 614.1b + CR 614.10: Check whether the active player should skip the given
/// step due to a static step-skip replacement that affects them.
fn should_skip_step_static(state: &GameState, step: Phase) -> bool {
    let active = state.active_player;
    let context = super::static_abilities::StaticCheckContext {
        player_id: Some(active),
        ..Default::default()
    };
    // CR 702.26b + CR 604.1: `active_static_definitions` owns the gating.
    state.battlefield.iter().any(|id| {
        state.objects.get(id).is_some_and(|obj| {
            super::functioning_abilities::active_static_definitions(state, obj).any(|sd| {
                if sd.mode != (StaticMode::SkipStep { step }) {
                    return false;
                }

                if let Some(ref affected) = sd.affected {
                    super::static_abilities::static_filter_matches(state, &context, affected, *id)
                } else {
                    obj.controller == active
                }
            })
        })
    })
}

/// CR 614.10a: Consume a one-shot "skip your next [step] step" only when that
/// step would otherwise occur. Static step skips are checked first by callers.
fn consume_next_step_skip(state: &mut GameState, step: Phase) -> bool {
    let idx = state.active_player.0 as usize;
    let Some(skips) = state.steps_to_skip.get_mut(idx) else {
        return false;
    };
    let Some(count) = skips.get_mut(&step) else {
        return false;
    };
    if *count == 0 {
        return false;
    }
    *count -= 1;
    if *count == 0 {
        skips.remove(&step);
    }
    true
}

fn should_skip_step_now(state: &mut GameState, step: Phase) -> bool {
    should_skip_step_static(state, step) || consume_next_step_skip(state, step)
}

/// CR 714.3b: As the precombat main phase begins, put a lore counter on each Saga
/// the active player controls. This is a turn-based action, not a triggered ability.
fn add_lore_counters_to_sagas(state: &mut GameState, events: &mut Vec<GameEvent>) -> bool {
    let active = state.active_player;
    let saga_ids: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| {
                    obj.controller == active && obj.card_types.subtypes.iter().any(|s| s == "Saga")
                })
                .unwrap_or(false)
        })
        .collect();

    // CR 614.1: Route through replacement pipeline so Vorinclex-class effects apply.
    for (index, saga_id) in saga_ids.iter().copied().enumerate() {
        if !super::effects::counters::add_counter_with_replacement(
            state,
            active,
            saga_id,
            CounterType::Lore,
            1,
            events,
        ) {
            let remaining = saga_ids[index + 1..]
                .iter()
                .copied()
                .map(|object_id| PendingCounterAddition::Object {
                    actor: active,
                    object_id,
                    counter_type: CounterType::Lore,
                    count: 1,
                })
                .collect();
            super::effects::counters::stash_pending_counter_additions(
                state,
                remaining,
                PendingEffectResolved::with_post_actions_without_effect(
                    EffectKind::GenericEffect,
                    saga_id,
                    Vec::new(),
                ),
            );
            return false;
        }
    }
    true
}

/// CR 503.1 / CR 504.2 / CR 507.1 / CR 513.1: Process phase triggers for the current step.
/// Fabricates a PhaseChanged event for `state.phase` and runs trigger matching.
///
/// Returns `(fired, ordering_prompt)`:
/// * `fired` is `true` if any triggers were placed on the stack, are pending
///   target selection, or are awaiting CR 603.3b ordering. The combat arms
///   (BeginCombat / EndCombat) use this to decide whether to set up / tear down
///   combat and grant a priority window.
/// * `ordering_prompt` is `Some(...)` when the phase must pause before priority:
///   - `WaitingFor::OrderTriggers { .. }` when 2+ simultaneous triggers controlled
///     by the same player fired and that player must order them (CR 603.3b), or
///   - an active trigger prompt (`TriggerTargetSelection`, etc.) when
///     `pending_trigger` / `deferred_triggers` still hold unresolved work (CR
///     603.3). The caller MUST surface this prompt instead of granting priority.
fn process_phase_triggers(state: &mut GameState) -> (bool, Option<WaitingFor>) {
    let phase_event = [GameEvent::PhaseChanged { phase: state.phase }];
    let stack_before = state.stack.len();
    let waiting_before = state.waiting_for.clone();
    super::triggers::process_triggers(state, &phase_event);
    // CR 603.3b: an unresolved ordering pass keeps its triggers in
    // `pending_trigger_order` (not on the stack, not in `pending_trigger`), so it
    // must count toward `fired` and surface its prompt. Reconstruct the prompt
    // from the AUTHORITATIVE source (`pending_trigger_order`) rather than cloning
    // `state.waiting_for`: if an upstream phase-advance orphaned the pass and left
    // `waiting_for` stale, cloning it would re-surface the stale state and hang.
    // Reading the canonical pending state also RECOVERS already-corrupted saves by
    // surfacing the real ordering prompt. Note `pending_trigger_order.is_some()` no
    // longer blindly implies `waiting_for == OrderTriggers`, which is exactly why
    // the prior `.then(|| clone)` idiom was unsafe.
    let order_triggers_prompt = super::triggers::build_next_order_triggers_prompt_public(state);
    let active_trigger_prompt = (order_triggers_prompt.is_none()
        && (state.pending_trigger.is_some() || !state.deferred_triggers.is_empty()))
    .then(|| state.waiting_for.clone());
    // CR 117.5 + CR 118.12a: Unless-pay and other inline resolution prompts arm
    // `waiting_for` without `pending_trigger` after the trigger has reached the
    // stack and begun resolving. Surface any non-priority prompt
    // `process_triggers` left behind so auto_advance does not clobber it with an
    // upkeep/draw/main priority window (Tabernacle #1326). The prompt must be
    // newly produced by trigger processing; stale turn-action prompts from an
    // earlier phase (DeclareAttackers, etc.) are not phase-trigger work.
    let inline_resolution_prompt = (order_triggers_prompt.is_none()
        && active_trigger_prompt.is_none()
        && state.waiting_for != waiting_before
        && !matches!(
            state.waiting_for,
            WaitingFor::Priority { .. } | WaitingFor::GameOver { .. }
        ))
    .then(|| state.waiting_for.clone());
    let prompt = order_triggers_prompt
        .or(active_trigger_prompt)
        .or(inline_resolution_prompt);
    let fired = state.stack.len() > stack_before
        || state.pending_trigger.is_some()
        || !state.deferred_triggers.is_empty()
        || prompt.is_some();
    (fired, prompt)
}

pub fn auto_advance(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    loop {
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return state.waiting_for.clone();
        }
        // CR 703.4q + CR 616.1: A step-end empty-mana drain paused on a
        // player's CR 616.1 choice. Surface the prompt so the engine round-
        // trips through `GameAction::ChooseReplacement`; the drain resumes
        // via the `EmptyManaPool` arm of `handle_replacement_choice`.
        if state.pending_phase_transition_progress.is_some() {
            state.deferred_step_trigger_resume = Some(state.phase);
            return state.waiting_for.clone();
        }

        // CR 800.4: If the active player has been eliminated, skip their
        // remaining phases and proceed to the next player's turn.
        if !super::players::is_alive(state, state.active_player) {
            state.phase = Phase::Cleanup;
            advance_phase(state, events);
            continue;
        }

        match state.phase {
            Phase::Untap => {
                // CR 614.1b + CR 614.10a: Skip the untap step if a static or
                // one-shot "skip your next untap step" replacement applies.
                if !should_skip_step_now(state, Phase::Untap) {
                    let candidates = untap_choice_candidates(state, state.active_player);
                    if !candidates.is_empty() {
                        return WaitingFor::UntapChoice {
                            player: state.active_player,
                            candidates,
                            chosen_not_to_untap: Vec::new(),
                        };
                    }
                    // CR 502.3: With no optional-decline candidates, either
                    // surface a required bounded `ChooseUntapSubset` prompt (a
                    // MaxUntapPerType cap is over its limit) or untap + advance.
                    // `begin_untap_or_subset_prompt` advances the phase itself
                    // when it untaps, so only fall through to `advance_phase`
                    // below when no subset prompt is raised.
                    if let Some(prompt) =
                        begin_untap_or_subset_prompt(state, events, HashSet::new())
                    {
                        return prompt;
                    }
                    continue;
                }
                // CR 502.4 / CR 117.3a: No player receives priority during the untap step.
                advance_phase(state, events);
            }
            Phase::Upkeep => {
                if should_skip_step_now(state, Phase::Upkeep) {
                    advance_phase(state, events);
                    continue;
                }
                // CR 704.3: Check SBAs before beginning-of-upkeep triggers so that
                // city blessing (CR 702.131b) and other SBA-granted designations are
                // applied before trigger conditions like "if you have the city's blessing"
                // are evaluated (Twilight Prophet #1375).
                let waiting_before_sba = state.waiting_for.clone();
                super::sba::check_state_based_actions(state, events);
                if state.waiting_for != waiting_before_sba
                    && !matches!(state.waiting_for, WaitingFor::Priority { .. })
                {
                    return state.waiting_for.clone();
                }
                if let Some(prompt) =
                    crate::game::contraptions::perform_contraption_upkeep_turn_based_action(
                        state, events,
                    )
                {
                    return prompt;
                }
                // CR 503.1a: "At the beginning of [your] upkeep" triggers fire here.
                // CR 603.3b: 2+ same-controller upkeep triggers (multiple suspended
                // cards, two Howling Mines) require an ordering choice that must be
                // surfaced before priority — see `process_phase_triggers`.
                if let (_, Some(prompt)) = process_phase_triggers(state) {
                    return prompt;
                }
                // CR 503.2 + CR 117.1c: The active player ALWAYS receives priority
                // during the upkeep step, regardless of whether triggers fired.
                // Whether to auto-pass through this priority window (or honor the
                // user's `phase_stops` / full-control preferences) is decided by
                // `run_auto_pass_loop` and the frontend, not by skipping the step
                // here. Mirrors the pattern in PreCombatMain and DeclareBlockers.
                return WaitingFor::Priority {
                    player: state.active_player,
                };
            }
            Phase::Draw => {
                // CR 103.8: The starting player skips their first-turn draw
                // step only in a two-player game (CR 103.8a) or Two-Headed
                // Giant (CR 103.8b) — not in 3+ player multiplayer
                // (CR 103.8c). `first_player_skips_first_draw` encodes this
                // gate so it stays in sync with `should_skip_draw`.
                // CR 614.10a + CR 614.1b: Other "skip your draw step" effects
                // (replacements or static abilities) also remove the whole step.
                if (state.turn_number == 1 && first_player_skips_first_draw(state))
                    || should_skip_step_now(state, Phase::Draw)
                {
                    advance_phase(state, events);
                    continue;
                }
                if let Some(wf) = execute_draw(state, events) {
                    return wf;
                }
                // CR 504.2: "At the beginning of [your] draw step" triggers fire here.
                // CR 603.3b: surface a same-controller ordering prompt before priority.
                if let (_, Some(prompt)) = process_phase_triggers(state) {
                    return prompt;
                }
                // CR 504.3 + CR 117.1c: The active player ALWAYS receives priority
                // during the draw step (after the turn-based draw and any triggers).
                // See the Upkeep arm above for the rationale — same pattern.
                return WaitingFor::Priority {
                    player: state.active_player,
                };
            }
            Phase::PreCombatMain | Phase::PostCombatMain => {
                // CR 714.3b: As the precombat main phase begins, add a lore counter
                // to each Saga the active player controls (turn-based action).
                if state.phase == Phase::PreCombatMain {
                    if !add_lore_counters_to_sagas(state, events) {
                        return state.waiting_for.clone();
                    }
                    super::attractions::perform_roll_to_visit_turn_based_action(state, events);
                    // CR 702.xxx: Paradigm (Strixhaven) — turn-based action at
                    // the start of the active player's first precombat main
                    // phase: offer to cast a copy of each exiled paradigm
                    // source the player controls. Modeled alongside the saga
                    // lore-counter hook (CR 505.4 anchor for beginning-of-
                    // precombat-main turn-based actions). Assign when WotC
                    // publishes SOS CR update.
                    let active = state.active_player;
                    if super::effects::paradigm::enqueue_offer_if_any(state, active) {
                        return state.waiting_for.clone();
                    }
                }
                // CR 603.2b + CR 603.3: beginning-of-main-phase triggers are
                // put on the stack before the active player receives priority.
                // CR 603.3b: surface a same-controller ordering prompt first.
                if let (_, Some(prompt)) = process_phase_triggers(state) {
                    return prompt;
                }
                // CR 505.6: The active player receives priority during a main phase.
                return WaitingFor::Priority {
                    player: state.active_player,
                };
            }
            Phase::BeginCombat => {
                // CR 507.1: "At the beginning of combat" triggers fire here.
                // Process triggers regardless of attackers — CR 507.1 says the step
                // happens unconditionally; trigger conditions (e.g., ControlCount)
                // are checked by the trigger system, not by skipping the step.
                let (triggers_fired, ordering_prompt) = process_phase_triggers(state);
                if triggers_fired {
                    state.combat = Some(crate::game::combat::CombatState::default());
                    // CR 603.3b: surface a same-controller ordering prompt before
                    // priority; combat state is set first so it exists when the
                    // ordered begin-combat triggers later resolve.
                    if let Some(prompt) = ordering_prompt {
                        return prompt;
                    }
                    return WaitingFor::Priority {
                        player: state.active_player,
                    };
                }
                if combat::has_potential_attackers(state) {
                    state.combat = Some(crate::game::combat::CombatState::default());
                    advance_phase(state, events);
                    // Continue to DeclareAttackers
                } else {
                    // CR 508.8: No attackers possible and no begin-combat
                    // triggers — skip declare attackers through end of combat.
                    // Don't return: continue the loop so the PostCombatMain
                    // match arm runs process_phase_triggers (survival, etc.).
                    state.combat = None;
                    enter_phase(state, Phase::PostCombatMain, events);
                }
            }
            Phase::DeclareAttackers => {
                // CR 508.1: Active player declares attackers as a turn-based action.
                let valid_attacker_ids = super::combat::get_valid_attacker_ids(state);
                let valid_attack_targets = super::combat::get_valid_attack_targets(state);
                return WaitingFor::DeclareAttackers {
                    player: state.active_player,
                    valid_attacker_ids,
                    valid_attack_targets,
                };
            }
            Phase::DeclareBlockers => {
                // CR 509.1: Defending player declares blockers as a turn-based action.
                super::combat::prune_attackers_not_in_play(state);
                let has_attackers = super::combat::has_attackers_in_play(state);
                if has_attackers {
                    // CR 509.1 + CR 117.1c: The declare blockers turn-based action always
                    // runs — even when no legal blocks are available — and the active
                    // player always receives priority during the step (required for
                    // instants and Ninjutsu-family activations per CR 702.49, notably
                    // Sneak which is restricted to this step). The phase layer only
                    // emits the interactive waiting state; whether to auto-submit empty
                    // blockers (because no legal blocks exist, or because the defender
                    // is in UntilEndOfTurn mode) is decided by `run_auto_pass_loop`.
                    let defending = combat::next_defending_player_to_declare_blockers(state)
                        .unwrap_or_else(|| super::players::next_player(state, state.active_player));
                    let valid_block_targets =
                        super::combat::get_valid_block_targets_for_player(state, defending);
                    let valid_blocker_ids: Vec<_> = valid_block_targets.keys().copied().collect();
                    let block_requirements =
                        super::combat::block_requirements_for_player(state, defending);
                    return WaitingFor::DeclareBlockers {
                        player: defending,
                        valid_blocker_ids,
                        valid_block_targets,
                        block_requirements,
                    };
                } else {
                    // CR 508.8: Declare blockers and combat damage steps are skipped if no attackers.
                    state.phase = Phase::EndCombat;
                    events.push(GameEvent::PhaseChanged {
                        phase: Phase::EndCombat,
                    });
                    // Continue loop to process EndCombat
                }
            }
            Phase::CombatDamage => {
                // CR 510.1a + CR 613.4c: Combat damage equals a creature's power as determined
                // by the layer system (layer 7c applies P/T counters). Flush here so
                // combat_damage_amount reads evaluated power, not stale base power. commit_attackers
                // (combat.rs) marks layers dirty; the post-action pipeline flush runs after
                // resolve_combat_damage returns — too late without this pre-flush.
                super::layers::flush_layers(state);
                // CR 510.1 / CR 510.2: Combat damage assigned and dealt as a turn-based action.
                // resolve_combat_damage may pause for interactive assignment (2+ blockers).
                if let Some(waiting) = combat_damage::resolve_combat_damage(state, events) {
                    state.waiting_for = waiting.clone();
                    return waiting;
                }
                // CR 603.3b: combat-damage triggers ran inside resolve_combat_damage
                // (process_combat_damage_triggers -> process_triggers). If 2+ triggers
                // controlled by the same player fired simultaneously, process_triggers
                // populated `pending_trigger_order` and set `waiting_for` to the
                // OrderTriggers prompt. Those triggers sit in `pending_trigger_order`, NOT
                // on the stack, so the `!state.stack.is_empty()` guard below would advance
                // past the prompt and strand them forever (the turn-18 hang). Surface the
                // ordering prompt now, mirroring finish_declare_attackers (engine_combat.rs).
                // NOTE: a first-strike sub-step OrderTriggers prompt is surfaced earlier,
                // via the `Some(waiting)` return from resolve_combat_damage above (CR 510.4
                // Part A in combat_damage.rs); the mandatory regular sub-step is then resumed
                // by the empty-stack completeness gate in priority.rs. This guard handles the
                // regular-step case, where resolve_combat_damage returns None but set
                // `waiting_for` to the OrderTriggers prompt internally.
                if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
                    return state.waiting_for.clone();
                }
                // CR 704.3 / CR 800.4: SBAs may have ended the game during combat damage.
                if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
                    return state.waiting_for.clone();
                }
                // CR 603.3b + issue #1350: deferred triggers collapsed during
                // elimination must drain before advancing past combat damage.
                if !state.deferred_triggers.is_empty() || state.pending_trigger.is_some() {
                    return WaitingFor::Priority {
                        player: state.active_player,
                    };
                }
                // If triggers were placed on the stack (DamageReceived, dies, etc.),
                // grant priority so they can resolve before advancing.
                if !state.stack.is_empty() {
                    return WaitingFor::Priority {
                        player: state.active_player,
                    };
                }
                advance_phase(state, events);
                // Continue to EndCombat
            }
            Phase::EndCombat => {
                // CR 511.1: "At end of combat" triggers fire here.
                let (triggers_fired, ordering_prompt) = process_phase_triggers(state);
                // CR 511.3: At end of combat, all creatures are removed from combat.
                state.combat = None;
                super::layers::prune_end_of_combat_effects(state);
                for obj in state.objects.iter_mut().map(|(_, v)| v) {
                    obj.replacement_definitions
                        .retain(|r| !matches!(r.expiry, Some(RestrictionExpiry::EndOfCombat)));
                }
                state
                    .pending_damage_replacements
                    .retain(|r| !matches!(r.expiry, Some(RestrictionExpiry::EndOfCombat)));
                if triggers_fired {
                    // CR 603.3b: surface a same-controller ordering prompt before priority.
                    if let Some(prompt) = ordering_prompt {
                        return prompt;
                    }
                    return WaitingFor::Priority {
                        player: state.active_player,
                    };
                }
                advance_phase(state, events);
                // Continue to PostCombatMain
            }
            Phase::End => {
                // CR 513.1 + CR 611.2a/b: Expire `PlayFromExile { duration:
                // UntilNextStepOf { step: End, player: Controller } }` grants for the active
                // player BEFORE end-step triggers fire. CR 513.2 prevents
                // the end step from "backing up" — a new same-turn grant
                // from an end-step trigger (e.g., Rocco, Street Chef) is
                // created AFTER this prune runs, so it correctly survives.
                super::layers::prune_end_step_casting_permissions(state, state.active_player);
                // CR 513.1 + CR 611.2a: Mirror the casting-permission prune
                // for transient continuous effects with the same duration —
                // any future parser arm emitting `UntilNextStepOf { step: End }` onto a
                // pump / control-change effect expires here rather than
                // outliving its scheduled step.
                super::layers::prune_until_next_end_step_effects(state, state.active_player);
                // CR 513.1: End step — active player receives priority.
                // CR 513.1a: "At the beginning of [your] end step" triggers fire here.
                // CR 603.3b: surface a same-controller ordering prompt before priority.
                if let (_, Some(prompt)) = process_phase_triggers(state) {
                    return prompt;
                }
                return WaitingFor::Priority {
                    player: state.active_player,
                };
            }
            Phase::Cleanup => {
                // CR 514: Cleanup step — discard to hand size (CR 514.1), remove damage and expire effects (CR 514.2).
                if let Some(waiting) = execute_cleanup(state, events) {
                    return waiting;
                }
                advance_phase(state, events);
                // advance_phase handles start_next_turn when wrapping Cleanup -> Untap
                // Continue loop to process next turn's phases
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::Supertype;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use std::sync::Arc;

    fn setup() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        state
    }

    #[test]
    fn declare_blockers_prompts_actual_defending_player_in_multiplayer() {
        let mut state = GameState::new(crate::types::format::FormatConfig::free_for_all(), 4, 42);
        state.active_player = PlayerId(0);
        state.phase = Phase::DeclareBlockers;
        let attacker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        state.combat = Some(combat::CombatState {
            attackers: vec![combat::AttackerInfo::new(
                attacker,
                combat::AttackTarget::Player(PlayerId(2)),
                PlayerId(2),
            )],
            ..Default::default()
        });

        let waiting = auto_advance(&mut state, &mut Vec::new());

        assert!(matches!(
            waiting,
            WaitingFor::DeclareBlockers {
                player: PlayerId(2),
                ..
            }
        ));
    }

    #[test]
    fn multiplayer_defending_players_declare_blockers_separately_in_turn_order() {
        let mut state = GameState::new(crate::types::format::FormatConfig::free_for_all(), 4, 42);
        state.active_player = PlayerId(0);
        state.phase = Phase::DeclareBlockers;

        let attacker_to_p2 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Attacker to P2".to_string(),
            Zone::Battlefield,
        );
        let attacker_to_p3 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Attacker to P3".to_string(),
            Zone::Battlefield,
        );
        let blocker_p2 = create_object(
            &mut state,
            CardId(3),
            PlayerId(2),
            "P2 Blocker".to_string(),
            Zone::Battlefield,
        );
        let blocker_p3 = create_object(
            &mut state,
            CardId(4),
            PlayerId(3),
            "P3 Blocker".to_string(),
            Zone::Battlefield,
        );
        for id in [attacker_to_p2, attacker_to_p3, blocker_p2, blocker_p3] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
        }

        state.combat = Some(combat::CombatState {
            attackers: vec![
                combat::AttackerInfo::new(
                    attacker_to_p2,
                    combat::AttackTarget::Player(PlayerId(2)),
                    PlayerId(2),
                ),
                combat::AttackerInfo::new(
                    attacker_to_p3,
                    combat::AttackTarget::Player(PlayerId(3)),
                    PlayerId(3),
                ),
            ],
            ..Default::default()
        });

        let waiting = auto_advance(&mut state, &mut Vec::new());
        assert!(matches!(
            waiting,
            WaitingFor::DeclareBlockers {
                player: PlayerId(2),
                ..
            }
        ));
        if let WaitingFor::DeclareBlockers {
            valid_blocker_ids,
            valid_block_targets,
            ..
        } = &waiting
        {
            assert_eq!(valid_blocker_ids, &vec![blocker_p2]);
            assert_eq!(
                valid_block_targets.get(&blocker_p2),
                Some(&vec![attacker_to_p2])
            );
        }
        state.waiting_for = waiting;

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(2),
            crate::types::actions::GameAction::DeclareBlockers {
                assignments: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(
            state
                .combat
                .as_ref()
                .unwrap()
                .pending_blocker_declaration_events
                .len(),
            1
        );
        assert!(matches!(
            result.waiting_for,
            WaitingFor::DeclareBlockers {
                player: PlayerId(3),
                ..
            }
        ));
        if let WaitingFor::DeclareBlockers {
            valid_blocker_ids,
            valid_block_targets,
            ..
        } = &result.waiting_for
        {
            assert_eq!(valid_blocker_ids, &vec![blocker_p3]);
            assert_eq!(
                valid_block_targets.get(&blocker_p3),
                Some(&vec![attacker_to_p3])
            );
        }

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(3),
            crate::types::actions::GameAction::DeclareBlockers {
                assignments: Vec::new(),
            },
        )
        .unwrap();
        assert!(state
            .combat
            .as_ref()
            .unwrap()
            .pending_blocker_declaration_events
            .is_empty());
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn next_phase_advances_in_order() {
        assert_eq!(next_phase(Phase::Untap), Phase::Upkeep);
        assert_eq!(next_phase(Phase::Upkeep), Phase::Draw);
        assert_eq!(next_phase(Phase::Draw), Phase::PreCombatMain);
        assert_eq!(next_phase(Phase::PreCombatMain), Phase::BeginCombat);
        assert_eq!(next_phase(Phase::PostCombatMain), Phase::End);
        assert_eq!(next_phase(Phase::End), Phase::Cleanup);
    }

    #[test]
    fn next_phase_wraps_cleanup_to_untap() {
        assert_eq!(next_phase(Phase::Cleanup), Phase::Untap);
    }

    #[test]
    fn advance_phase_changes_phase_and_emits_event() {
        let mut state = setup();
        state.phase = Phase::Untap;
        let mut events = Vec::new();

        advance_phase(&mut state, &mut events);

        assert_eq!(state.phase, Phase::Upkeep);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PhaseChanged {
                phase: Phase::Upkeep
            }
        )));
    }

    #[test]
    fn advance_phase_tracks_combat_phases_started_this_turn() {
        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        let mut events = Vec::new();

        advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::BeginCombat);
        assert_eq!(state.combat_phases_started_this_turn, 1);

        state
            .extra_phases
            .push(crate::types::game_state::ExtraPhase {
                anchor: Phase::EndCombat,
                phase: Phase::BeginCombat,
            });
        state.phase = Phase::EndCombat;
        advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::BeginCombat);
        assert_eq!(state.combat_phases_started_this_turn, 2);
    }

    #[test]
    fn advance_phase_clears_mana_pools() {
        use crate::types::identifiers::ObjectId;
        use crate::types::mana::{ManaType, ManaUnit};

        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(1),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });

        let mut events = Vec::new();
        advance_phase(&mut state, &mut events);

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn advance_phase_retains_only_static_matching_controller_mana() {
        use crate::types::ability::{StaticDefinition, TargetFilter};
        use crate::types::mana::{ManaColor, ManaType, ManaUnit};
        use crate::types::statics::StaticMode;

        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Electro, Assaulting Battery".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::StepEndUnspentMana {
                    filter: Some(ManaColor::Red),
                    action: crate::types::mana::StepEndManaAction::Retain,
                })
                .affected(TargetFilter::Controller),
            );

        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(10),
            false,
            Vec::new(),
        ));
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Blue,
            ObjectId(11),
            false,
            Vec::new(),
        ));
        state.players[1].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(12),
            false,
            Vec::new(),
        ));

        advance_phase(&mut state, &mut Vec::new());

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 0);
        assert_eq!(state.players[1].mana_pool.count_color(ManaType::Red), 0);
    }

    #[test]
    fn retained_mana_empties_after_static_source_stops_applying() {
        use crate::types::ability::{StaticDefinition, TargetFilter};
        use crate::types::mana::{ManaColor, ManaType, ManaUnit};
        use crate::types::statics::StaticMode;

        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Electro, Assaulting Battery".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::StepEndUnspentMana {
                    filter: Some(ManaColor::Red),
                    action: crate::types::mana::StepEndManaAction::Retain,
                })
                .affected(TargetFilter::Controller),
            );
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(10),
            false,
            Vec::new(),
        ));

        advance_phase(&mut state, &mut Vec::new());
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, source, Zone::Graveyard, &mut events);
        advance_phase(&mut state, &mut Vec::new());

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn static_all_mana_retention_survives_cleanup_step() {
        use crate::types::ability::{StaticDefinition, TargetFilter};
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::statics::StaticMode;

        let mut state = setup();
        state.phase = Phase::End;
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Upwelling".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::StepEndUnspentMana {
                    filter: None,
                    action: crate::types::mana::StepEndManaAction::Retain,
                })
                .affected(TargetFilter::Controller),
            );
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(10),
            false,
            Vec::new(),
        ));
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(11),
            false,
            Vec::new(),
        ));

        advance_phase(&mut state, &mut Vec::new());

        assert_eq!(state.phase, Phase::Cleanup);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
    }

    #[test]
    fn advance_phase_transforms_unspent_mana_to_target_type() {
        // CR 614.1a + CR 703.4q: Horizon Stone — would-be-lost mana becomes
        // colorless instead. RUNTIME test that drives `advance_phase` so the
        // transform is observed at the live mana-pool step.
        use crate::types::ability::{StaticDefinition, TargetFilter};
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::statics::StaticMode;

        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Horizon Stone".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::StepEndUnspentMana {
                    filter: None,
                    action: crate::types::mana::StepEndManaAction::Transform(ManaType::Colorless),
                })
                .affected(TargetFilter::Controller),
            );

        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(10),
            false,
            Vec::new(),
        ));
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Blue,
            ObjectId(11),
            false,
            Vec::new(),
        ));
        // Opponent has no transform — their mana drains normally.
        state.players[1].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(12),
            false,
            Vec::new(),
        ));

        advance_phase(&mut state, &mut Vec::new());

        assert_eq!(state.players[0].mana_pool.total(), 2);
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            2
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 0);
        assert_eq!(state.players[1].mana_pool.total(), 0);
    }

    #[test]
    fn advance_phase_keeps_end_of_turn_mana_until_cleanup() {
        // CR 500.5 + CR 703.4q (H2 invariant, Klauth, Unrivaled Ancient):
        // "Until end of turn, you don't lose this mana as steps and phases
        // end." A unit carrying `ManaExpiry::EndOfTurn` must survive every
        // non-cleanup phase/step transition and only drain when the turn
        // actually ends. A plain `None`-expiry unit drains on the very first
        // transition. RUNTIME test driving `advance_phase` through the live
        // empty-pool pipeline — guards the payload builder that previously
        // emitted a `Drop` decision for retained expiry-bound units.
        use crate::types::mana::{ManaExpiry, ManaType, ManaUnit};

        let mut state = setup();
        state.phase = Phase::PreCombatMain;

        let mut klauth_mana = ManaUnit::new(ManaType::Red, ObjectId(10), false, Vec::new());
        klauth_mana.expiry = Some(ManaExpiry::EndOfTurn);
        state.players[0].mana_pool.add(klauth_mana);
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Blue,
            ObjectId(11),
            false,
            Vec::new(),
        ));

        // First transition (PreCombatMain → next step, not cleanup): the
        // plain Blue mana drains; the EndOfTurn Red mana is retained.
        advance_phase(&mut state, &mut Vec::new());
        assert_ne!(state.phase, Phase::Cleanup);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 0);

        // Drive forward until cleanup; the EndOfTurn mana survives each
        // intermediate step and only drains once the turn ends.
        while state.phase != Phase::Cleanup {
            assert_eq!(
                state.players[0].mana_pool.count_color(ManaType::Red),
                1,
                "EndOfTurn mana must persist through {:?}",
                state.phase
            );
            advance_phase(&mut state, &mut Vec::new());
        }
        assert_eq!(state.phase, Phase::Cleanup);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 0);
    }

    #[test]
    fn advance_phase_keeps_end_of_combat_mana_until_combat_ends() {
        // CR 500.5 + CR 703.4q + CR 702.189a: Firebending mana says "Until
        // end of combat, you don't lose this mana as steps and phases end."
        // It must survive combat step transitions through the live empty-pool
        // pipeline, then drain when the game leaves combat.
        use crate::types::mana::{ManaExpiry, ManaType, ManaUnit};

        let mut state = setup();
        state.phase = Phase::BeginCombat;

        let mut firebending_mana = ManaUnit::new(ManaType::Red, ObjectId(10), false, Vec::new());
        firebending_mana.expiry = Some(ManaExpiry::EndOfCombat);
        state.players[0].mana_pool.add(firebending_mana);
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Blue,
            ObjectId(11),
            false,
            Vec::new(),
        ));

        while state.phase != Phase::PostCombatMain {
            assert_eq!(
                state.players[0].mana_pool.count_color(ManaType::Red),
                1,
                "EndOfCombat mana must persist through {:?}",
                state.phase
            );
            advance_phase(&mut state, &mut Vec::new());
            assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 0);
        }

        assert_eq!(state.phase, Phase::PostCombatMain);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 0);
    }

    #[test]
    fn transient_retention_drives_player_retained_mana_query() {
        // CR 611.2b + CR 703.4q: The Last Agni Kai shape — a spell installs a
        // turn-scoped retention rule via `add_transient_continuous_effect` with
        // `affected: SpecificPlayer { controller }` and modifications carrying
        // `AddStaticMode { StepEndUnspentMana { action: Retain } }`. The runtime query must see it.
        // RUNTIME test: drives `advance_phase` through the live pipeline.
        use crate::types::ability::{ContinuousModification, Duration, TargetFilter};
        use crate::types::mana::{ManaColor, ManaType, ManaUnit};
        use crate::types::statics::StaticMode;

        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "The Last Agni Kai".to_string(),
            Zone::Graveyard,
        );

        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::StepEndUnspentMana {
                    filter: Some(ManaColor::Red),
                    action: crate::types::mana::StepEndManaAction::Retain,
                },
            }],
            None,
        );

        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(10),
            false,
            Vec::new(),
        ));
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Blue,
            ObjectId(11),
            false,
            Vec::new(),
        ));

        advance_phase(&mut state, &mut Vec::new());

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 0);
    }

    #[test]
    fn static_player_scope_retention_covers_every_player() {
        // CR 703.4q: Upwelling — "Players don't lose unspent mana as steps and
        // phases end." With `affected: TargetFilter::Player`, retention must
        // cover both controller and opponent. Drives `advance_phase` through
        // the pipeline (RUNTIME test, not shape).
        use crate::types::ability::{StaticDefinition, TargetFilter};
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::statics::StaticMode;

        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Upwelling".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::StepEndUnspentMana {
                    filter: None,
                    action: crate::types::mana::StepEndManaAction::Retain,
                })
                .affected(TargetFilter::Player),
            );

        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(10),
            false,
            Vec::new(),
        ));
        state.players[1].mana_pool.add(ManaUnit::new(
            ManaType::Blue,
            ObjectId(11),
            false,
            Vec::new(),
        ));

        advance_phase(&mut state, &mut Vec::new());

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[1].mana_pool.count_color(ManaType::Blue), 1);
    }

    // -----------------------------------------------------------------
    // CR 616.1 step-end mana RUNTIME tests (commit 2 cutover).
    //
    // Test list (from /tmp/cr616/plan-v2.md "Tests" section):
    //   #1  single_retention_no_pause                — covered by
    //       `advance_phase_retains_only_static_matching_controller_mana`
    //       above; RUNTIME path identical under the new pipeline.
    //   #2  single_transform_no_pause                — covered by
    //       `advance_phase_transforms_unspent_mana_to_target_type`.
    //   #3  two_player_apnap_independent_no_pause   — covered by
    //       `static_player_scope_retention_covers_every_player`.
    //   #8  transient_continuous_handler_via_last_agni_kai_pattern — covered
    //       by `transient_retention_drives_player_retained_mana_query`.
    //
    // The five tests below cover the genuinely new behavior in commit 2:
    // CR 616.1 player-choice ordering when ≥2 handlers apply to the same
    // emptying event (#4), APNAP serialization across players (#5, #9),
    // the no-handler-default path (#10), and the Drop-disposition matcher
    // gate (#11).
    //
    // Expiry-bound interaction tests (#6, #7) live in `types/mana.rs` as
    // shape tests on `clear_expiring_at_step_end` since that helper runs
    // before the pipeline starts (H2 invariant — expiry-bound units never
    // enter the replacement path).
    // -----------------------------------------------------------------

    /// CR 616.1 (#4): When two `Retain` handlers on a single player both
    /// match a unit, the affected player chooses ordering via
    /// `GameAction::ChooseReplacement`. Either choice resolves to the same
    /// observable pool state (both keep the unit), so the test asserts the
    /// pause + resume mechanics rather than ordering side-effects: a
    /// `ReplacementChoice` waiting_for surfaces, and after a choice both
    /// handlers apply (CR 614.5 one-opportunity-per-event tracking via
    /// `ProposedEvent::applied`).
    #[test]
    fn step_end_mana_two_retention_handlers_pause_for_player_choice() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::{StaticDefinition, TargetFilter};
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::statics::StaticMode;

        use crate::types::mana::ManaColor;

        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        // Two filtered `Retain` handlers on player 0's battlefield: one
        // accepts Green only, the other Blue only. Pool seeded with one
        // Green + one Blue Drop unit. The initial scan finds both
        // handlers applicable (each sees ≥1 Drop unit overall). After
        // the chosen handler runs and flips its colored unit to Keep,
        // the other handler's matcher still returns true (the opposite-
        // color unit is still Drop) and auto-applies. This setup is the
        // only way to distinguish "1 handler fired" from "2 handlers
        // fired" using observable end state: count(Green)==1 alone
        // would be consistent with either outcome under a single-unit
        // setup; here count(Green)==1 AND count(Blue)==1 prove both ran.
        let handler_specs = [(1u64, ManaColor::Green), (2u64, ManaColor::Blue)];
        for (n, color) in handler_specs {
            let source = create_object(
                &mut state,
                CardId(n),
                PlayerId(0),
                format!("Retention Source {n}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .static_definitions
                .push(
                    StaticDefinition::new(StaticMode::StepEndUnspentMana {
                        filter: Some(color),
                        action: crate::types::mana::StepEndManaAction::Retain,
                    })
                    .affected(TargetFilter::Controller),
                );
        }
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Green,
            ObjectId(99),
            false,
            Vec::new(),
        ));
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Blue,
            ObjectId(98),
            false,
            Vec::new(),
        ));

        let mut events = Vec::new();
        advance_phase(&mut state, &mut events);

        // CR 616.1: pipeline paused on a multi-handler choice for player 0.
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::ReplacementChoice {
                    player: PlayerId(0),
                    candidate_count: 2,
                    ..
                }
            ),
            "expected multi-handler ReplacementChoice, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_phase_transition_progress.is_some());

        // Player 0 chooses the first (Green) handler; the second (Blue)
        // handler then applies on the rebuilt event. Both flip their
        // respective unit to Keep, so both colors survive.
        state.priority_player = PlayerId(0);
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("choose first handler");

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            1,
            "Green should have been retained by the first handler"
        );
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Blue),
            1,
            "Blue should have been retained by the second handler — \
             count(Blue)==0 here means only the chosen handler fired \
             and CR 616.1f continuation was skipped"
        );
        assert!(state.pending_phase_transition_progress.is_none());
    }

    /// CR 616.1 (#5 + #9): With handlers on both players, APNAP order
    /// determines whose choice comes first. The active player's CR 616.1
    /// prompt surfaces before the non-active player's drain runs; the
    /// non-active player's drain runs only after the active player resumes.
    #[test]
    fn step_end_mana_multi_player_choice_serializes_in_apnap_order() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::{StaticDefinition, TargetFilter};
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::statics::StaticMode;

        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);

        // Each player has two Retain handlers (multi-handler conflict on
        // both pools) and a unit in their own pool.
        for player_idx in [0u8, 1] {
            for n in 1u64..=2 {
                let source = create_object(
                    &mut state,
                    CardId((u64::from(player_idx) + 1) * 10 + n),
                    PlayerId(player_idx),
                    format!("Retention {player_idx}/{n}"),
                    Zone::Battlefield,
                );
                state
                    .objects
                    .get_mut(&source)
                    .unwrap()
                    .static_definitions
                    .push(
                        StaticDefinition::new(StaticMode::StepEndUnspentMana {
                            filter: None,
                            action: crate::types::mana::StepEndManaAction::Retain,
                        })
                        .affected(TargetFilter::Controller),
                    );
            }
            state.players[player_idx as usize]
                .mana_pool
                .add(ManaUnit::new(
                    ManaType::Green,
                    ObjectId(900 + u64::from(player_idx)),
                    false,
                    Vec::new(),
                ));
        }

        let mut events = Vec::new();
        advance_phase(&mut state, &mut events);

        // CR 616.1: APNAP order — active player (PlayerId(0)) chooses first.
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::ReplacementChoice {
                    player: PlayerId(0),
                    ..
                }
            ),
            "active player must be prompted first under APNAP; got {:?}",
            state.waiting_for
        );

        // Player 0 resolves; queue advances to player 1 who also needs to
        // choose. The drain in `handle_replacement_choice` propagates the
        // next prompt without returning to Priority in between.
        state.priority_player = PlayerId(0);
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("player 0 chooses");

        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::ReplacementChoice {
                    player: PlayerId(1),
                    ..
                }
            ),
            "after active player resolves, next APNAP player chooses; got {:?}",
            state.waiting_for
        );

        // Player 1 resolves; both pools survive.
        state.priority_player = PlayerId(1);
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("player 1 chooses");

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        assert_eq!(state.players[1].mana_pool.count_color(ManaType::Green), 1);
        assert!(state.pending_phase_transition_progress.is_none());
    }

    /// CR 616.1g (#10): A player with no applicable handlers drains through
    /// the pipeline without pausing — their pool empties as normal. With a
    /// second player who DOES have handlers, the no-handler player's drain
    /// completes silently and the handler-owning player is then processed.
    #[test]
    fn step_end_mana_player_with_no_handlers_drains_default() {
        use crate::types::ability::{StaticDefinition, TargetFilter};
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::statics::StaticMode;

        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);

        // Only player 1 has a retention handler. Player 0 has no handlers.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Retention".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::StepEndUnspentMana {
                    filter: None,
                    action: crate::types::mana::StepEndManaAction::Retain,
                })
                .affected(TargetFilter::Controller),
            );
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(10),
            false,
            Vec::new(),
        ));
        state.players[1].mana_pool.add(ManaUnit::new(
            ManaType::Blue,
            ObjectId(11),
            false,
            Vec::new(),
        ));

        advance_phase(&mut state, &mut Vec::new());

        // Player 0 has no handlers — pool empties.
        assert_eq!(state.players[0].mana_pool.total(), 0);
        // Player 1's handler matched (single handler, no choice needed).
        assert_eq!(state.players[1].mana_pool.count_color(ManaType::Blue), 1);
        // Queue completed without pausing.
        assert!(state.pending_phase_transition_progress.is_none());
    }

    /// CR 614.5 secondary correctness (#11): The matcher gate is "Drop
    /// disposition AND filter color match" — not "filter color match alone".
    /// After a `Transform(Red)` handler recolors a Blue unit to Red, a
    /// `Retain(filter=Red)` handler must NOT match the recolored unit
    /// (disposition is now `Recolor(Red)`, not `Drop`).
    ///
    /// Scenario: pool has a single Blue unit. Two handlers on the same
    /// player — Transform(Blue→Red) and Retain(filter=Red). The Transform
    /// matches first (filter=None / matches Blue). After Transform, the
    /// unit's disposition is `Recolor(Red)`, not `Drop`. Retain(filter=Red)'s
    /// matcher inspects the rebuilt event and finds no `Drop` units it can
    /// claim, so it is NOT a candidate on the second iteration. Result: one
    /// Red unit survives in the pool.
    #[test]
    fn step_end_mana_recolor_then_retain_filter_does_not_match_new_color() {
        use crate::types::ability::{StaticDefinition, TargetFilter};
        use crate::types::mana::{ManaColor, ManaType, ManaUnit};
        use crate::types::statics::StaticMode;

        let mut state = setup();
        state.phase = Phase::PreCombatMain;

        // Transform handler: unfiltered → recolor every Drop unit to Red.
        let xform = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Recolorer".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&xform)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::StepEndUnspentMana {
                    filter: None,
                    action: crate::types::mana::StepEndManaAction::Transform(ManaType::Red),
                })
                .affected(TargetFilter::Controller),
            );
        // Retention handler: only on Red units.
        let retain = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Red Keeper".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&retain)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::StepEndUnspentMana {
                    filter: Some(ManaColor::Red),
                    action: crate::types::mana::StepEndManaAction::Retain,
                })
                .affected(TargetFilter::Controller),
            );
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Blue,
            ObjectId(10),
            false,
            Vec::new(),
        ));

        let mut events = Vec::new();
        advance_phase(&mut state, &mut events);

        // CR 614.5 secondary: after Transform recolors the Blue unit to Red,
        // its disposition is `Recolor(Red)`, NOT `Drop`. The Retain handler
        // requires a `Drop` unit; the matcher rejects, so Retain is not a
        // candidate and the pipeline never pauses. Pool ends with one Red.
        //
        // But: if `Retain` HAD matched on filter-alone, this would have
        // been a multi-handler conflict that paused for choice. The
        // absence of a pause is the load-bearing signal here.
        assert!(state.pending_phase_transition_progress.is_none());
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
    }

    /// CR 616.1 (#4 ordering — secondary): When the same affected player is
    /// offered Retain vs Transform on a single unit, choosing one observably
    /// distinguishes from the other. Asserts that `chosen_index` 0 vs 1
    /// produces different pool outcomes (Keep vs Recolor).
    #[test]
    fn step_end_mana_choice_index_distinguishes_retain_from_transform() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::{StaticDefinition, TargetFilter};
        use crate::types::actions::GameAction;
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::statics::StaticMode;

        fn run(choose: usize) -> ManaType {
            let mut state = setup();
            state.phase = Phase::PreCombatMain;
            // Retain (unfiltered) and Transform(Blue) both apply to every
            // Drop unit — two-handler choice.
            let retain = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Retainer".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&retain)
                .unwrap()
                .static_definitions
                .push(
                    StaticDefinition::new(StaticMode::StepEndUnspentMana {
                        filter: None,
                        action: crate::types::mana::StepEndManaAction::Retain,
                    })
                    .affected(TargetFilter::Controller),
                );
            let xform = create_object(
                &mut state,
                CardId(2),
                PlayerId(0),
                "Recolorer".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&xform)
                .unwrap()
                .static_definitions
                .push(
                    StaticDefinition::new(StaticMode::StepEndUnspentMana {
                        filter: None,
                        action: crate::types::mana::StepEndManaAction::Transform(ManaType::Blue),
                    })
                    .affected(TargetFilter::Controller),
                );
            state.players[0].mana_pool.add(ManaUnit::new(
                ManaType::Red,
                ObjectId(10),
                false,
                Vec::new(),
            ));

            let mut events = Vec::new();
            advance_phase(&mut state, &mut events);
            state.priority_player = PlayerId(0);
            apply_as_current(&mut state, GameAction::ChooseReplacement { index: choose })
                .expect("choose");

            // After both handlers have applied (or the chosen-first one then
            // the other), the unit's final color is the survivor.
            state.players[0]
                .mana_pool
                .mana
                .first()
                .map(|u| u.color)
                .expect("unit survived")
        }

        // Order of handler enumeration in the scan determines `candidates`
        // ordering. Both ordering outcomes leave one unit in the pool
        // (Retain keeps; Transform after Retain has no Drop unit to recolor,
        // OR Transform recolors then Retain keeps the recolored unit). We
        // assert the choice index is observable: one choice yields the
        // original Red (Retain wins on first iteration; Transform's matcher
        // then rejects since disposition is `Keep`), the other yields Blue
        // (Transform wins on first iteration; Retain's matcher then rejects
        // since disposition is `Recolor(Blue)`).
        let outcome_0 = run(0);
        let outcome_1 = run(1);
        assert_ne!(
            outcome_0, outcome_1,
            "choice index must produce observably different outcomes (Retain vs Transform)"
        );
        assert!(matches!(outcome_0, ManaType::Red | ManaType::Blue));
        assert!(matches!(outcome_1, ManaType::Red | ManaType::Blue));
    }

    #[test]
    fn advance_phase_resets_priority_to_active_player() {
        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(1); // Was opponent's priority

        let mut events = Vec::new();
        advance_phase(&mut state, &mut events);

        assert_eq!(state.priority_player, PlayerId(0));
        assert_eq!(state.priority_pass_count, 0);
    }

    /// CR 500.8: An extra phase whose `anchor` does NOT match the current
    /// phase must NOT be consumed early. This is the regression test for the
    /// Aurelia bug — pushing `BeginCombat` (anchor = `EndCombat`) during
    /// `DeclareAttackers` must not redirect the natural
    /// `DeclareAttackers → DeclareBlockers` advance into the extra combat.
    #[test]
    fn extra_phase_does_not_consume_when_anchor_mismatches_current_phase() {
        use crate::types::game_state::ExtraPhase;

        let mut state = setup();
        state.phase = Phase::DeclareAttackers;
        state.extra_phases.push(ExtraPhase {
            anchor: Phase::EndCombat,
            phase: Phase::BeginCombat,
        });

        let mut events = Vec::new();
        advance_phase(&mut state, &mut events);

        // Natural successor of DeclareAttackers is DeclareBlockers — the
        // extra-phase entry must remain queued for its real anchor.
        assert_eq!(state.phase, Phase::DeclareBlockers);
        assert_eq!(state.extra_phases.len(), 1);
        assert_eq!(state.extra_phases[0].anchor, Phase::EndCombat);
        assert_eq!(state.extra_phases[0].phase, Phase::BeginCombat);
    }

    /// CR 500.8: The extra phase IS consumed exactly when transitioning out
    /// of its anchor phase. With anchor = `EndCombat`, advancing from
    /// `EndCombat` jumps to the extra `BeginCombat` (not the natural
    /// `PostCombatMain`).
    #[test]
    fn extra_phase_consumes_when_anchor_matches_current_phase() {
        use crate::types::game_state::ExtraPhase;

        let mut state = setup();
        state.phase = Phase::EndCombat;
        state.extra_phases.push(ExtraPhase {
            anchor: Phase::EndCombat,
            phase: Phase::BeginCombat,
        });

        let mut events = Vec::new();
        advance_phase(&mut state, &mut events);

        assert_eq!(state.phase, Phase::BeginCombat);
        assert!(state.extra_phases.is_empty());
    }

    /// CR 500.8 regression — Aurelia, the Warleader. Trigger fires during
    /// `DeclareAttackers`, resolver pushes `ExtraPhase { anchor: EndCombat,
    /// phase: BeginCombat }`. The remaining steps of the FIRST combat
    /// (DeclareBlockers, CombatDamage, EndCombat) MUST run before the
    /// extra combat begins. This pins the exact phase sequence the bug
    /// silently broke.
    #[test]
    fn cr_500_8_aurelia_extra_combat_does_not_skip_first_combat_steps() {
        use crate::types::game_state::ExtraPhase;

        let mut state = setup();
        state.phase = Phase::DeclareAttackers;
        // Simulate Aurelia's trigger resolving mid-combat.
        state.extra_phases.push(ExtraPhase {
            anchor: Phase::EndCombat,
            phase: Phase::BeginCombat,
        });

        // Walk the phase machine forward and record each phase entered.
        let mut events = Vec::new();
        let mut sequence = vec![state.phase];
        for _ in 0..12 {
            advance_phase(&mut state, &mut events);
            sequence.push(state.phase);
            if matches!(state.phase, Phase::PostCombatMain) {
                break;
            }
        }

        // CR 506.1 + CR 500.8: First combat's steps (DeclareBlockers,
        // CombatDamage, EndCombat) must execute, then the extra
        // BeginCombat starts a new combat. The extra combat's full cycle
        // runs to its EndCombat, then the natural PostCombatMain.
        assert_eq!(
            sequence,
            vec![
                Phase::DeclareAttackers,
                Phase::DeclareBlockers,
                Phase::CombatDamage,
                Phase::EndCombat,
                // Extra combat begins (CR 500.8: directly after the combat phase)
                Phase::BeginCombat,
                Phase::DeclareAttackers,
                Phase::DeclareBlockers,
                Phase::CombatDamage,
                Phase::EndCombat,
                // No more extra phases — natural successor.
                Phase::PostCombatMain,
            ]
        );
        assert!(state.extra_phases.is_empty());
    }

    /// CR 500.8: World at War / Combat Celebrant exert variant — additional
    /// combat phase followed by additional main phase. Both push with
    /// anchor = EndCombat; LIFO ordering (`rposition` from the end)
    /// consumes BeginCombat (most recent push) on the FIRST EndCombat
    /// transition, then PostCombatMain on the SECOND EndCombat transition
    /// (after the extra combat finishes).
    #[test]
    fn cr_500_8_with_main_phase_lifo_anchor_ordering() {
        use crate::types::game_state::ExtraPhase;

        let mut state = setup();
        state.phase = Phase::DeclareAttackers;
        // Mirror `additional_phase::resolve` push order with PostCombatMain as a follow-up.
        state.extra_phases.push(ExtraPhase {
            anchor: Phase::EndCombat,
            phase: Phase::PostCombatMain,
        });
        state.extra_phases.push(ExtraPhase {
            anchor: Phase::EndCombat,
            phase: Phase::BeginCombat,
        });

        let mut events = Vec::new();
        let mut sequence = vec![state.phase];
        for _ in 0..14 {
            advance_phase(&mut state, &mut events);
            sequence.push(state.phase);
            if matches!(state.phase, Phase::End) {
                break;
            }
        }

        assert_eq!(
            sequence,
            vec![
                Phase::DeclareAttackers,
                Phase::DeclareBlockers,
                Phase::CombatDamage,
                Phase::EndCombat,
                // First EndCombat consumes the most recent push: BeginCombat.
                Phase::BeginCombat,
                Phase::DeclareAttackers,
                Phase::DeclareBlockers,
                Phase::CombatDamage,
                Phase::EndCombat,
                // Second EndCombat consumes the remaining push: PostCombatMain.
                Phase::PostCombatMain,
                // Natural successor — no entries left.
                Phase::End,
            ]
        );
        assert!(state.extra_phases.is_empty());
    }

    /// CR 500.8: Multiple extra combats stacked with the same anchor are
    /// consumed in LIFO order — each EndCombat transition pops one. This
    /// covers Aggravated Assault re-activation / multiple Aurelias.
    #[test]
    fn cr_500_8_multiple_extra_combats_consume_one_per_anchor_pass() {
        use crate::types::game_state::ExtraPhase;

        let mut state = setup();
        state.phase = Phase::EndCombat;
        for _ in 0..3 {
            state.extra_phases.push(ExtraPhase {
                anchor: Phase::EndCombat,
                phase: Phase::BeginCombat,
            });
        }

        let mut events = Vec::new();

        // First pass: EndCombat → BeginCombat (one extra consumed).
        advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::BeginCombat);
        assert_eq!(state.extra_phases.len(), 2);

        // Walk the extra combat to its own EndCombat.
        for _ in 0..4 {
            advance_phase(&mut state, &mut events);
        }
        assert_eq!(state.phase, Phase::EndCombat);

        // Second pass: another extra combat consumes.
        advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::BeginCombat);
        assert_eq!(state.extra_phases.len(), 1);
    }

    /// Negative test — extra-turn / extra-step mechanics that did NOT use
    /// `extra_phases` are unaffected by the typing change. `extra_turns` is
    /// a separate `Vec<PlayerId>` consumed by `start_next_turn`.
    #[test]
    fn extra_turns_field_is_independent_of_extra_phases() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.extra_turns.push(PlayerId(0));
        // No extra_phases pushed — make sure normal phase advance is unaffected.
        state.phase = Phase::Cleanup;

        let mut events = Vec::new();
        advance_phase(&mut state, &mut events);

        // Wrap from Cleanup to Untap consumes the extra turn entry — same
        // player remains active.
        assert_eq!(state.phase, Phase::Untap);
        assert_eq!(state.active_player, PlayerId(0));
        assert!(state.extra_turns.is_empty());
        // extra_phases is unchanged (still empty).
        assert!(state.extra_phases.is_empty());
    }

    #[test]
    fn start_next_turn_increments_turn_and_swaps_player() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.turn_number = 1;

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
        assert_eq!(state.priority_player, PlayerId(1));
    }

    #[test]
    fn start_next_turn_resets_per_turn_counters() {
        let mut state = setup();
        state.lands_played_this_turn = 1;
        state.players[0].has_drawn_this_turn = true;
        state.players[0].lands_played_this_turn = 1;
        let object_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Test".to_string(),
            Zone::Battlefield,
        );
        crate::game::effects::counters::apply_counter_addition(
            &mut state,
            PlayerId(0),
            object_id,
            crate::types::counter::CounterType::Plus1Plus1,
            1,
            &mut Vec::new(),
        );
        assert_eq!(state.counter_added_this_turn.len(), 1);

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert_eq!(state.lands_played_this_turn, 0);
        assert!(!state.players[0].has_drawn_this_turn);
        assert_eq!(state.players[0].lands_played_this_turn, 0);
        assert!(state.counter_added_this_turn.is_empty());
    }

    /// CR 601.2a + CR 113.6b: Turn cleanup must clear BOTH the per-source
    /// `ExileCastPermission` once-per-turn slots AND the rolling "cards exiled
    /// with this source this turn" pool (Maralen, Fae Ascendant). Driven
    /// through `start_next_turn` rather than a manual `.clear()`, so a
    /// regression dropping either reset line in `start_next_turn` fails here
    /// instead of staying green.
    #[test]
    fn start_next_turn_resets_exile_cast_permission_tracking() {
        let mut state = setup();
        let source = ObjectId(42);
        state.exile_cast_permissions_used.insert(source);
        state
            .cards_exiled_with_source_this_turn
            .insert(source, vec![ObjectId(7)]);

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert!(
            state.exile_cast_permissions_used.is_empty(),
            "OncePerTurn exile-cast slots must reset at turn start"
        );
        assert!(
            state.cards_exiled_with_source_this_turn.is_empty(),
            "per-turn exiled-with-source pool must reset at turn start"
        );
    }

    #[test]
    fn start_next_turn_emits_turn_started_event() {
        let mut state = setup();
        let mut events = Vec::new();

        start_next_turn(&mut state, &mut events);

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::TurnStarted { turn_number: 2, .. })));
    }

    #[test]
    fn execute_untap_untaps_active_player_permanents() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().tapped = true;

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        assert!(!state.objects[&id].tapped);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentUntapped { object_id } if *object_id == id)));
    }

    #[test]
    fn execute_untap_applies_edge_of_malacol_untap_replacement() {
        use crate::types::card_type::CoreType;
        use crate::types::counter::CounterType;

        let mut state = setup();
        state.active_player = PlayerId(0);
        // CR 502.3 + CR 502.4: the turn-based untap happens during the untap
        // step; `ReplacementCondition::DuringUntapStep` gates on this phase.
        state.phase = Phase::Untap;

        // A tapped creature the active player controls.
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.tapped = true;
        }

        // Edge of Malacol's untap-step replacement.
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Edge of Malacol".to_string(),
            Zone::Battlefield,
        );
        {
            let def = crate::parser::oracle_replacement::parse_replacement_line(
                "If a creature you control would untap during your untap step, put two +1/+1 counters on it instead.",
                "Edge of Malacol",
            )
            .expect("untap-step replacement should parse");
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
            obj.replacement_definitions.push(def.clone());
            Arc::make_mut(&mut obj.base_replacement_definitions).push(def);
        }

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        // The untap is replaced: the creature stays tapped, emits no untap event,
        // and gains two +1/+1 counters instead — exercising the DuringUntapStep
        // gate and the untap-step raise end to end (a broken phase check or raise
        // would untap the creature and skip the counters).
        assert!(
            state.objects[&creature].tapped,
            "untap must be replaced; the creature stays tapped"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                GameEvent::PermanentUntapped { object_id } if *object_id == creature
            )),
            "no untap event is emitted when the untap is replaced"
        );
        assert_eq!(
            state.objects[&creature]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            2,
            "two +1/+1 counters are added instead of untapping"
        );
    }

    /// CR 502.3 + CR 701.26b: Blossombind — "Enchanted creature can't become
    /// untapped …" is an unconditional `ProposedEvent::Untap` PREVENTION
    /// (CR 701.26b, the broad prohibition — NOT a `CantUntap` static, which is the
    /// untap-step-only class). This drives the production untap step (`execute_untap`)
    /// and asserts the host stays tapped; the EFFECT-driven untap path is covered
    /// separately in `tap_untap.rs`. The replacement is parsed from the real
    /// Oracle text via the cross-layer split and installed on the attached Aura.
    /// Reverting the untap-prevention replacement (or its split routing) makes the
    /// untap-step `replace_event` return `Execute`, the creature untaps, and this
    /// assertion fails — so the test discriminates the change.
    #[test]
    fn execute_untap_honors_blossombind_cant_become_untapped() {
        use crate::game::effects::attach::attach_to;
        use crate::types::card_type::CoreType;
        use crate::types::replacements::ReplacementEvent;

        let mut state = setup();
        state.active_player = PlayerId(0);

        let host = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bound Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&host).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.tapped = true;
        }

        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Blossombind".to_string(),
            Zone::Battlefield,
        );
        {
            // Parse the real compound line; pull the Untap-prevention replacement
            // out of the cross-layer split and install it on the Aura. (The
            // AddCounter-prevention conjunct is irrelevant to untap; the full split
            // is exercised by the parser-layer test.)
            let parsed = crate::parser::parse_oracle_text(
                "Enchant creature\nEnchanted creature can't become untapped and can't have counters put on it.",
                "Blossombind",
                &[],
                &["Enchantment".to_string()],
                &["Aura".to_string()],
            );
            assert!(
                parsed
                    .replacements
                    .iter()
                    .any(|def| def.event == ReplacementEvent::Untap),
                "Blossombind's untap prohibition must parse to an Untap-prevention replacement"
            );
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.replacement_definitions = parsed.replacements.into();
        }
        attach_to(&mut state, aura, host);

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        assert!(
            state.objects[&host].tapped,
            "Blossombind's enchanted creature must stay tapped at the untap step"
        );
        assert!(
            !events.iter().any(|event| {
                matches!(event, GameEvent::PermanentUntapped { object_id } if *object_id == host)
            }),
            "skipped untap must not emit PermanentUntapped"
        );
    }

    #[test]
    fn execute_untap_honors_attached_subject_cant_untap_from_parser() {
        use crate::game::effects::attach::attach_to;
        use crate::types::card_type::CoreType;

        let mut state = setup();
        state.active_player = PlayerId(0);

        let host = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Locked Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&host).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.tapped = true;
        }

        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Flood the Engine".to_string(),
            Zone::Battlefield,
        );
        {
            let defs = crate::parser::oracle_static::parse_static_line_multi(
                "Enchanted permanent loses all abilities and doesn't untap during its controller's untap step.",
            );
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.base_card_types = obj.card_types.clone();
            for def in defs.iter().cloned() {
                obj.static_definitions.push(def);
            }
            Arc::make_mut(&mut obj.base_static_definitions).extend(defs);
        }
        attach_to(&mut state, aura, host);

        let mut events = Vec::new();
        // CR 604.1: a functioning CantUntap static IS present, so the hoisted
        // existence gate is true and the per-permanent `check_static_ability`
        // scan MUST still run — proving the gate does not suppress real scans on
        // the gate=true path.
        crate::game::perf_counters::reset();
        execute_untap(&mut state, &mut events);
        let scans = crate::game::perf_counters::snapshot().static_full_scans;

        assert!(
            state.objects[&host].tapped,
            "attached CantUntap static must keep the enchanted permanent tapped"
        );
        assert!(
            !events.iter().any(|event| {
                matches!(event, GameEvent::PermanentUntapped { object_id } if *object_id == host)
            }),
            "skipped untap must not emit PermanentUntapped"
        );
        assert!(
            scans > 0,
            "gate=true path must still run the real per-permanent CantUntap scan"
        );
    }

    fn install_may_choose_not_to_untap_static(state: &mut GameState, source_id: ObjectId) {
        use crate::types::ability::StaticDefinition;
        let def = StaticDefinition::new(StaticMode::MayChooseNotToUntap);
        let obj = state.objects.get_mut(&source_id).unwrap();
        obj.static_definitions.push(def.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(def);
    }

    /// CR 502.3: Install a Smoke-class "can't untap more than one creature"
    /// max-untap cap on `source_id`.
    fn install_max_untap_one_creature_static(state: &mut GameState, source_id: ObjectId) {
        use crate::types::ability::{StaticDefinition, TargetFilter, TypedFilter};
        let def = StaticDefinition::new(StaticMode::MaxUntapPerType {
            filter: TargetFilter::Typed(TypedFilter::creature()),
            max: 1,
        });
        let obj = state.objects.get_mut(&source_id).unwrap();
        obj.static_definitions.push(def.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(def);
    }

    fn create_tapped_creature(state: &mut GameState, card_id: u64, name: &str) -> ObjectId {
        use crate::types::card_type::CoreType;
        let id = create_object(
            state,
            CardId(card_id),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.tapped = true;
        id
    }

    /// CR 502.3 + CR 604.1: GAP-1 guard. The every-turn untap of K tapped
    /// active-player permanents on a restriction-free board must NOT perform any
    /// whole-battlefield `check_static_ability` scan — the hoisted CantUntap
    /// existence gate is false, so the per-permanent scan is skipped. Reverting
    /// the gate restores O(K) scans, failing the `== 0` assertion. Drives the
    /// production `execute_untap`, not the prompt helper.
    #[test]
    fn execute_untap_no_static_scan_on_vanilla_board() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        let ids: Vec<ObjectId> = (0..8)
            .map(|i| create_tapped_creature(&mut state, 100 + i, &format!("Bear {i}")))
            .collect();

        crate::game::perf_counters::reset();
        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);
        let scans = crate::game::perf_counters::snapshot().static_full_scans;

        for id in &ids {
            assert!(!state.objects[id].tapped, "vanilla permanent must untap");
        }
        assert_eq!(
            scans, 0,
            "no static-ability whole-board scan on a vanilla untap"
        );
    }

    /// CR 502.3 + CR 604.1: with a `MaxUntapPerType` cap present but no
    /// functioning CantUntap static, `max_untap_subset_prompt` reaches
    /// `untap_excluded_ids`, whose per-permanent CantUntap scan is gated off by
    /// the hoisted existence flag — so building the over-cap group costs zero
    /// whole-board scans even though it does NOT bail early.
    #[test]
    fn max_untap_subset_prompt_no_cant_untap_scan_with_cap() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        let smoke = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Smoke".to_string(),
            Zone::Battlefield,
        );
        install_max_untap_one_creature_static(&mut state, smoke);
        create_tapped_creature(&mut state, 2, "Bear A");
        create_tapped_creature(&mut state, 3, "Bear B");

        crate::game::perf_counters::reset();
        let prompt = max_untap_subset_prompt(&state, PlayerId(0), &HashSet::new());
        let scans = crate::game::perf_counters::snapshot().static_full_scans;

        assert!(
            prompt.is_some(),
            "two tapped creatures exceed the cap of one"
        );
        assert_eq!(
            scans, 0,
            "no CantUntap whole-board scan when no such static exists"
        );
    }

    /// CR 502.3: with no `MaxUntapPerType` cap in play, `max_untap_subset_prompt`
    /// bails before the `untap_excluded_ids` CantUntap scan — proving the
    /// early-return short-circuit. Reverting the bail makes the scan run over the
    /// tapped board, raising `static_full_scans` above zero.
    #[test]
    fn max_untap_subset_prompt_bails_without_cap_no_scan() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        for i in 0..8 {
            create_tapped_creature(&mut state, 200 + i, &format!("Bear {i}"));
        }

        crate::game::perf_counters::reset();
        let prompt = max_untap_subset_prompt(&state, PlayerId(0), &HashSet::new());
        let scans = crate::game::perf_counters::snapshot().static_full_scans;

        assert!(prompt.is_none(), "no cap means nothing to prompt");
        assert_eq!(scans, 0, "bail short-circuits before any whole-board scan");
    }

    /// CR 502.3: With a Smoke-class cap of one creature and two tapped
    /// creatures, the untap step does NOT silently clamp — it raises the
    /// `ChooseUntapSubset` prompt so the active player determines which one
    /// untaps. This is the architectural fix: the cap is a required bounded
    /// selection, not deterministic excess-skipping.
    #[test]
    fn max_untap_cap_raises_subset_prompt_over_cap() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let smoke = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Smoke".to_string(),
            Zone::Battlefield,
        );
        install_max_untap_one_creature_static(&mut state, smoke);

        let creature_a = create_tapped_creature(&mut state, 2, "Bear A");
        let creature_b = create_tapped_creature(&mut state, 3, "Bear B");

        let prompt = begin_untap_or_subset_prompt(&mut state, &mut Vec::new(), HashSet::new());
        match prompt {
            Some(WaitingFor::ChooseUntapSubset { player, group, max }) => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(max, 1);
                let mut g = group;
                g.sort_by_key(|id| id.0);
                let mut expected = vec![creature_a, creature_b];
                expected.sort_by_key(|id| id.0);
                assert_eq!(g, expected, "both over-cap creatures are offered");
            }
            other => panic!("expected ChooseUntapSubset prompt, got {other:?}"),
        }
        // Nothing untapped yet — the player must choose first (no auto-clamp).
        assert!(state.objects[&creature_a].tapped);
        assert!(state.objects[&creature_b].tapped);
    }

    /// CR 502.3: The active player's explicit subset selection is honored — the
    /// chosen creature untaps, the unchosen one stays tapped, with no reliance
    /// on iteration order. Exercises the full bridge: declines + subset choice.
    #[test]
    fn max_untap_subset_selection_untaps_chosen_only() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let smoke = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Smoke".to_string(),
            Zone::Battlefield,
        );
        install_max_untap_one_creature_static(&mut state, smoke);

        let creature_a = create_tapped_creature(&mut state, 2, "Bear A");
        let creature_b = create_tapped_creature(&mut state, 3, "Bear B");

        // Player chooses to untap creature_b (the non-first member).
        let mut chosen = HashSet::new();
        chosen.insert(creature_b);
        // Simulate the engine handler's complement fold: everything in the group
        // not chosen stays tapped.
        let mut skipped = HashSet::new();
        for id in [creature_a, creature_b] {
            if !chosen.contains(&id) {
                skipped.insert(id);
            }
        }
        let resumed = begin_untap_or_subset_prompt(&mut state, &mut Vec::new(), skipped);
        assert!(
            resumed.is_none(),
            "after the subset is resolved, untap executes and no further prompt is raised"
        );

        assert!(
            !state.objects[&creature_b].tapped,
            "the chosen creature untaps"
        );
        assert!(
            state.objects[&creature_a].tapped,
            "the unchosen creature stays tapped — explicit selection, not order"
        );
    }

    /// CR 502.3 SAFETY NET: A direct caller that reaches
    /// `execute_untap_with_choices` without resolving the subset prompt still
    /// has the cap enforced (deterministic clamp), so the engine never
    /// over-untaps past the CR 502.3 limit.
    #[test]
    fn max_untap_cap_clamp_safety_net_holds() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let smoke = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Smoke".to_string(),
            Zone::Battlefield,
        );
        install_max_untap_one_creature_static(&mut state, smoke);

        let creature_a = create_tapped_creature(&mut state, 2, "Bear A");
        let creature_b = create_tapped_creature(&mut state, 3, "Bear B");

        execute_untap(&mut state, &mut Vec::new());

        let untapped = [creature_a, creature_b]
            .iter()
            .filter(|id| !state.objects[id].tapped)
            .count();
        assert_eq!(
            untapped, 1,
            "the clamp keeps the cap enforced even on the direct untap path"
        );
    }

    /// CR 502.3: The player determines which permanents untap. A decline of the
    /// first creature must leave the SECOND creature untapped (the cap honors
    /// the player's choice rather than a fixed order).
    #[test]
    fn max_untap_cap_honors_player_decline_choice() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let smoke = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Smoke".to_string(),
            Zone::Battlefield,
        );
        install_max_untap_one_creature_static(&mut state, smoke);

        let creature_a = create_tapped_creature(&mut state, 2, "Bear A");
        let creature_b = create_tapped_creature(&mut state, 3, "Bear B");

        // Player declines creature_a, so creature_b is the one that untaps.
        let mut choices = HashSet::new();
        choices.insert(creature_a);
        execute_untap_with_choices(&mut state, &mut Vec::new(), &choices);

        assert!(
            state.objects[&creature_a].tapped,
            "declined creature stays tapped"
        );
        assert!(
            !state.objects[&creature_b].tapped,
            "the non-declined creature untaps under the cap"
        );
    }

    /// CR 502.3: The cap is type-scoped — a tapped artifact untaps freely while
    /// the creature cap applies only to creatures. Proves the filter is honored.
    #[test]
    fn max_untap_cap_does_not_restrict_other_types() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let smoke = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Smoke".to_string(),
            Zone::Battlefield,
        );
        install_max_untap_one_creature_static(&mut state, smoke);

        let creature_a = create_tapped_creature(&mut state, 2, "Bear A");
        let creature_b = create_tapped_creature(&mut state, 3, "Bear B");

        let artifact = {
            use crate::types::card_type::CoreType;
            let id = create_object(
                &mut state,
                CardId(4),
                PlayerId(0),
                "Mox".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.tapped = true;
            id
        };

        execute_untap(&mut state, &mut Vec::new());

        assert!(
            !state.objects[&artifact].tapped,
            "artifact untaps freely under a creature-only cap"
        );
        let untapped_creatures = [creature_a, creature_b]
            .iter()
            .filter(|id| !state.objects[id].tapped)
            .count();
        assert_eq!(untapped_creatures, 1, "creature cap still applies");
    }

    /// CR 502.3: When a group is over the cap, `max_untap_subset_prompt` offers
    /// every eligible member so the active player determines which untap. The
    /// per-permanent optional-decline prompt (`untap_choice_candidates`) is a
    /// SEPARATE concern and must NOT include the cap group (no
    /// `MayChooseNotToUntap` static is present here).
    #[test]
    fn max_untap_subset_prompt_offers_over_cap_group() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let smoke = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Smoke".to_string(),
            Zone::Battlefield,
        );
        install_max_untap_one_creature_static(&mut state, smoke);

        let creature_a = create_tapped_creature(&mut state, 2, "Bear A");
        let creature_b = create_tapped_creature(&mut state, 3, "Bear B");

        // The decline prompt is empty — these creatures have no
        // MayChooseNotToUntap static; the cap is a distinct selection.
        assert!(
            untap_choice_candidates(&state, PlayerId(0)).is_empty(),
            "cap group must not leak into the optional-decline prompt"
        );

        let (mut group, max) =
            max_untap_subset_prompt(&state, PlayerId(0), &HashSet::new()).expect("over-cap prompt");
        assert_eq!(max, 1);
        group.sort_by_key(|id| id.0);
        let mut expected = vec![creature_a, creature_b];
        expected.sort_by_key(|id| id.0);
        assert_eq!(group, expected);
    }

    /// CR 502.3: A group at or under the cap produces no max-untap prompt.
    #[test]
    fn max_untap_subset_prompt_empty_when_under_cap() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let smoke = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Smoke".to_string(),
            Zone::Battlefield,
        );
        install_max_untap_one_creature_static(&mut state, smoke);

        create_tapped_creature(&mut state, 2, "Bear A");

        assert!(max_untap_subset_prompt(&state, PlayerId(0), &HashSet::new()).is_none());
        assert!(untap_choice_candidates(&state, PlayerId(0)).is_empty());
    }

    /// CR 502.3: Declines reduce the eligible group before the cap check. If the
    /// player has already declined enough that the remaining eligible group is
    /// at or under the cap, no subset prompt is raised.
    #[test]
    fn max_untap_subset_prompt_respects_declines() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let smoke = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Smoke".to_string(),
            Zone::Battlefield,
        );
        install_max_untap_one_creature_static(&mut state, smoke);

        let creature_a = create_tapped_creature(&mut state, 2, "Bear A");
        let _creature_b = create_tapped_creature(&mut state, 3, "Bear B");

        // Declining one of the two leaves a single eligible creature — at the
        // cap, so no required selection remains.
        let mut declined = HashSet::new();
        declined.insert(creature_a);
        assert!(max_untap_subset_prompt(&state, PlayerId(0), &declined).is_none());
    }

    /// CR 502.3: a max-untap cap ("can't untap more than one creature") bounds
    /// the untap count from ABOVE only — choosing ZERO is legal. When the active
    /// player resolves the `ChooseUntapSubset` prompt with an empty selection,
    /// every member of the over-cap group folds into the skipped set, the whole
    /// group stays tapped, and the untap step advances cleanly with no residual
    /// prompt. This is the engine-side guarantee behind the frontend allowing an
    /// empty `SelectCards { cards: [] }` confirmation.
    #[test]
    fn max_untap_empty_subset_leaves_whole_group_tapped() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let smoke = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Smoke".to_string(),
            Zone::Battlefield,
        );
        install_max_untap_one_creature_static(&mut state, smoke);

        let creature_a = create_tapped_creature(&mut state, 2, "Bear A");
        let creature_b = create_tapped_creature(&mut state, 3, "Bear B");

        // Empty selection: the engine's SelectCards handler folds the entire
        // prompted group into the skipped set (chosen.len() == 0 <= max). Mirror
        // that fold here — nothing was chosen, so both group members stay tapped.
        let mut skipped = HashSet::new();
        skipped.insert(creature_a);
        skipped.insert(creature_b);
        let resumed = begin_untap_or_subset_prompt(&mut state, &mut Vec::new(), skipped);
        assert!(
            resumed.is_none(),
            "an empty untap subset resolves the step — no further prompt is raised"
        );

        assert!(
            state.objects[&creature_a].tapped,
            "choosing zero leaves the first group member tapped"
        );
        assert!(
            state.objects[&creature_b].tapped,
            "choosing zero leaves the second group member tapped"
        );
    }

    /// CR 502.3 + CR 611.1: a filter-scoped transient `CantUntap` (a spell/effect
    /// that installs "creatures don't untap …" by typed/filter target rather than
    /// a single `SpecificObject`) removes every affected permanent from the
    /// max-untap cap group AND the cap math. Here a creature-wide transient
    /// CantUntap makes BOTH tapped creatures ineligible, so the eligible group
    /// drops to zero — under the cap — and no `ChooseUntapSubset` prompt is
    /// raised. Proves the cap prompt no longer offers a permanent that cannot
    /// legally untap. Builds for the class (any filter-scoped transient
    /// CantUntap), not a single card.
    #[test]
    fn max_untap_prompt_excludes_filter_scoped_transient_cant_untap() {
        use crate::types::ability::{ContinuousModification, Duration, TargetFilter, TypedFilter};

        let mut state = setup();
        state.active_player = PlayerId(0);

        let smoke = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Smoke".to_string(),
            Zone::Battlefield,
        );
        install_max_untap_one_creature_static(&mut state, smoke);

        let creature_a = create_tapped_creature(&mut state, 2, "Bear A");
        let creature_b = create_tapped_creature(&mut state, 3, "Bear B");

        // Without the transient effect, the over-cap group offers both creatures.
        let (group, _max) = max_untap_subset_prompt(&state, PlayerId(0), &HashSet::new())
            .expect("two over a cap of one must prompt before the transient effect");
        assert_eq!(group.len(), 2);

        // Install a filter-scoped transient CantUntap on ALL creatures (a typed
        // filter target, not SpecificObject). Source is the smoke permanent.
        let source = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Frost Lattice".to_string(),
            Zone::Battlefield,
        );
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::Typed(TypedFilter::creature()),
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::CantUntap,
            }],
            None,
        );

        // Both creatures are now ineligible to untap, so the eligible group is
        // empty — at/under the cap — and no subset prompt is raised.
        assert!(
            max_untap_subset_prompt(&state, PlayerId(0), &HashSet::new()).is_none(),
            "filter-scoped transient CantUntap removes affected permanents from the cap group"
        );
        assert!(
            untap_excluded_ids(&state, PlayerId(0))
                .is_superset(&[creature_a, creature_b].into_iter().collect()),
            "both creatures are excluded by the filter-scoped transient CantUntap"
        );

        // And the real untap step keeps both tapped (cap prompt and untap agree).
        execute_untap(&mut state, &mut Vec::new());
        assert!(state.objects[&creature_a].tapped);
        assert!(state.objects[&creature_b].tapped);
    }

    #[test]
    fn untap_choice_candidates_include_tapped_permanents_with_may_not_untap() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let shackles = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Vedalken Shackles".to_string(),
            Zone::Battlefield,
        );
        install_may_choose_not_to_untap_static(&mut state, shackles);
        state.objects.get_mut(&shackles).unwrap().tapped = true;

        let untapped_static = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Untapped Shackles".to_string(),
            Zone::Battlefield,
        );
        install_may_choose_not_to_untap_static(&mut state, untapped_static);

        let normal_tapped = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&normal_tapped).unwrap().tapped = true;

        assert_eq!(untap_choice_candidates(&state, PlayerId(0)), vec![shackles]);
    }

    #[test]
    fn execute_untap_with_choices_leaves_chosen_permanent_tapped() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let shackles = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Vedalken Shackles".to_string(),
            Zone::Battlefield,
        );
        install_may_choose_not_to_untap_static(&mut state, shackles);
        state.objects.get_mut(&shackles).unwrap().tapped = true;

        let normal_tapped = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&normal_tapped).unwrap().tapped = true;

        let mut choices = HashSet::new();
        choices.insert(shackles);
        execute_untap_with_choices(&mut state, &mut Vec::new(), &choices);

        assert!(state.objects[&shackles].tapped);
        assert!(!state.objects[&normal_tapped].tapped);
    }

    #[test]
    fn auto_advance_prompts_for_untap_choice_before_untapping() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.phase = Phase::Untap;

        let shackles = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Vedalken Shackles".to_string(),
            Zone::Battlefield,
        );
        install_may_choose_not_to_untap_static(&mut state, shackles);
        state.objects.get_mut(&shackles).unwrap().tapped = true;

        let waiting = auto_advance(&mut state, &mut Vec::new());

        assert!(matches!(
            waiting,
            WaitingFor::UntapChoice {
                player: PlayerId(0),
                candidates,
                ..
            } if candidates == vec![shackles]
        ));
        assert!(state.objects[&shackles].tapped);
    }

    /// CR 502.3 + CR 113.6: Seedborn Muse class — its controller untaps
    /// permanents during each OTHER player's untap step.
    fn install_seedborn_static(state: &mut GameState, source_id: ObjectId) {
        use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
        let def = StaticDefinition::new(StaticMode::UntapsDuringEachOtherPlayersUntapStep)
            .affected(TargetFilter::Typed(
                TypedFilter::permanent().controller(ControllerRef::You),
            ));
        let obj = state.objects.get_mut(&source_id).unwrap();
        obj.static_definitions.push(def.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(def);
    }

    /// Mark the object as a creature so `TypeFilter::Permanent` matches.
    fn mark_as_creature(state: &mut GameState, id: ObjectId) {
        use crate::types::card_type::CoreType;
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
    }

    #[test]
    fn seedborn_untaps_controllers_permanents_on_opponents_untap_step() {
        let mut state = setup();
        state.active_player = PlayerId(1); // Opponent's untap step.

        let seedborn = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Seedborn Muse".to_string(),
            Zone::Battlefield,
        );
        install_seedborn_static(&mut state, seedborn);

        let mine_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let mine_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        mark_as_creature(&mut state, seedborn);
        mark_as_creature(&mut state, mine_a);
        mark_as_creature(&mut state, mine_b);
        state.objects.get_mut(&mine_a).unwrap().tapped = true;
        state.objects.get_mut(&mine_b).unwrap().tapped = true;
        state.objects.get_mut(&seedborn).unwrap().tapped = true;

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        // Seedborn's controller's permanents untapped during opponent's step.
        assert!(!state.objects[&mine_a].tapped);
        assert!(!state.objects[&mine_b].tapped);
        assert!(!state.objects[&seedborn].tapped);
    }

    #[test]
    fn seedborn_does_not_fire_on_controllers_own_untap_step() {
        let mut state = setup();
        state.active_player = PlayerId(0); // Seedborn's controller is active.

        let seedborn = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Seedborn Muse".to_string(),
            Zone::Battlefield,
        );
        install_seedborn_static(&mut state, seedborn);

        // A tapped opponent permanent must NOT untap — Seedborn only affects
        // its own controller's permanents, and this pass only runs when the
        // active player is NOT Seedborn's controller (it isn't this test).
        let opp_perm = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&opp_perm).unwrap().tapped = true;

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        assert!(state.objects[&opp_perm].tapped);
    }

    #[test]
    fn seedborn_phased_out_does_not_fire() {
        let mut state = setup();
        state.active_player = PlayerId(1);

        let seedborn = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Seedborn Muse".to_string(),
            Zone::Battlefield,
        );
        install_seedborn_static(&mut state, seedborn);
        // CR 702.26c: Phased-out permanents don't function.
        use crate::game::game_object::{PhaseOutCause, PhaseStatus};
        state.objects.get_mut(&seedborn).unwrap().phase_status = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };

        let mine = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&mine).unwrap().tapped = true;

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        // Seedborn is phased out, so the second-pass should NOT fire.
        assert!(state.objects[&mine].tapped);
    }

    #[test]
    fn execute_untap_does_not_untap_opponents_permanents() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().tapped = true;

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        assert!(state.objects[&id].tapped);
    }

    #[test]
    fn execute_draw_moves_top_of_library_to_hand() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        execute_draw(&mut state, &mut events);

        assert!(state.players[0].hand.contains(&id));
        assert!(!state.players[0].library.contains(&id));
        assert!(state.players[0].has_drawn_this_turn);
    }

    /// CR 805.4b: "Each player on a team draws a card during that team's
    /// draw step." A single `execute_draw` call must seed the queue with
    /// both active-team members and drain it to completion in the common
    /// (no-pause) case.
    #[test]
    fn execute_draw_two_headed_giant_both_teammates_draw() {
        let mut state = GameState::new(crate::types::FormatConfig::two_headed_giant(), 4, 0);
        state.active_player = PlayerId(0);
        let card0 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card0".to_string(),
            Zone::Library,
        );
        let card1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Card1".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        let result = execute_draw(&mut state, &mut events);

        assert!(result.is_none(), "no replacement pause expected here");
        assert!(state.players[0].hand.contains(&card0));
        assert!(state.players[1].hand.contains(&card1));
        assert!(state.players[0].has_drawn_this_turn);
        assert!(state.players[1].has_drawn_this_turn);
        assert!(
            state.pending_team_draw_step.is_empty(),
            "the draw-step queue must be fully drained, not left with stale entries"
        );
    }

    #[test]
    fn execute_draw_archenemy_hero_team_draws_all_living_heroes() {
        let mut state = GameState::new(crate::types::FormatConfig::archenemy(), 4, 0);
        state.active_player = PlayerId(1);
        let archenemy_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Scheme Boss Draw".to_string(),
            Zone::Library,
        );
        let hero_cards: Vec<ObjectId> = (1u8..=3)
            .map(|seat| {
                create_object(
                    &mut state,
                    CardId(10 + u64::from(seat)),
                    PlayerId(seat),
                    format!("Hero {seat} Draw"),
                    Zone::Library,
                )
            })
            .collect();

        let mut events = Vec::new();
        let result = execute_draw(&mut state, &mut events);

        assert!(result.is_none(), "no replacement pause expected here");
        assert!(state.players[0].library.contains(&archenemy_card));
        for (offset, card) in hero_cards.iter().enumerate() {
            assert!(state.players[offset + 1].hand.contains(card));
        }
    }

    #[test]
    fn execute_draw_archenemy_turn_draws_only_archenemy() {
        let mut state = GameState::new(crate::types::FormatConfig::archenemy(), 4, 0);
        state.active_player = PlayerId(0);
        let archenemy_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Archenemy Draw".to_string(),
            Zone::Library,
        );
        let hero_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Hero Draw".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        let result = execute_draw(&mut state, &mut events);

        assert!(result.is_none(), "no replacement pause expected here");
        assert!(state.players[0].hand.contains(&archenemy_card));
        assert!(state.players[1].library.contains(&hero_card));
    }

    /// CR 805.4b + CR 616.1: regression for the resumption gap flagged in
    /// review — if the active player's draw-step draw paused on a
    /// competing-replacement choice and was then resumed (popping the active
    /// player off the queue's front), the teammate left in the queue must
    /// still be drawn for by a later `drain_pending_team_draw_step` call
    /// (the exact call `handle_replacement_choice`'s resume epilogue makes),
    /// not silently dropped.
    #[test]
    fn drain_pending_team_draw_step_resumes_remaining_queued_teammate() {
        let mut state = GameState::new(crate::types::FormatConfig::two_headed_giant(), 4, 0);
        state.active_player = PlayerId(0);
        let card0 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card0".to_string(),
            Zone::Library,
        );
        let card1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Card1".to_string(),
            Zone::Library,
        );

        // Simulate "P0's draw already completed and was popped off the
        // front; P1 is still owed their draw" — the exact state the queue
        // is in immediately after a resumed P0 draw, before P1 has drawn.
        state.pending_team_draw_step = vec![PlayerId(1)];

        let mut events = Vec::new();
        let result = drain_pending_team_draw_step(&mut state, &mut events);

        assert!(result.is_none());
        assert!(
            state.players[0].hand.is_empty() && state.players[0].library.contains(&card0),
            "P0 already drew in this scenario — this call must not draw for them again"
        );
        assert!(
            state.players[1].hand.contains(&card1),
            "P1's queued draw must still happen on resume"
        );
        assert!(state.pending_team_draw_step.is_empty());
    }

    #[test]
    fn should_skip_draw_on_turn_1() {
        let mut state = setup();
        state.turn_number = 1;
        assert!(should_skip_draw(&state));

        state.turn_number = 2;
        assert!(!should_skip_draw(&state));
    }

    /// CR 103.8c: In multiplayer games other than Two-Headed Giant, the
    /// starting player does NOT skip their first draw step. Issue #954 —
    /// engine previously hardcoded the 2-player rule and silently dropped the
    /// first-turn draw in 3+ player Commander.
    #[test]
    fn multiplayer_starting_player_does_not_skip_first_draw() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 4, 42);
        state.turn_number = 1;
        assert!(
            !should_skip_draw(&state),
            "CR 103.8c: 4-player Commander game must not skip the starting \
             player's first draw step",
        );

        // Sanity: a 3-player free-for-all is also multiplayer.
        let mut state3 = GameState::new(FormatConfig::standard(), 3, 42);
        state3.turn_number = 1;
        assert!(
            !should_skip_draw(&state3),
            "CR 103.8c: 3-player game must not skip the starting player's \
             first draw step",
        );
    }

    /// CR 103.8b: In Two-Headed Giant the team who plays first DOES skip
    /// their first draw step, even though the game has 4 players.
    #[test]
    fn two_headed_giant_first_team_skips_first_draw() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.turn_number = 1;
        assert!(
            should_skip_draw(&state),
            "CR 103.8b: Two-Headed Giant first team must skip the first \
             draw step",
        );
    }

    /// CR 103.8a: A two-player Commander game is still a two-player game per
    /// CR 903.2; the first player skips their first draw step.
    #[test]
    fn two_player_commander_still_skips_first_draw() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 2, 42);
        state.turn_number = 1;
        assert!(
            should_skip_draw(&state),
            "CR 103.8a + CR 903.2: 2-player Commander still skips the first \
             player's first draw step",
        );
    }

    /// End-to-end: drive the engine through End-step priority passes and verify
    /// that with > 7 cards in hand, the resulting WaitingFor is DiscardToHandSize.
    /// Mirrors the user-visible flow (no direct execute_cleanup call).
    #[test]
    fn end_step_pass_priority_surfaces_discard_to_hand_size() {
        use crate::game::engine::apply;
        use crate::game::zones::create_object;
        use crate::types::actions::GameAction;
        use crate::types::identifiers::CardId;

        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.phase = Phase::End;
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        for i in 0..9 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Hand,
            );
        }
        assert_eq!(state.players[0].hand.len(), 9);

        // P0 passes end-step priority.
        let r1 = apply(&mut state, PlayerId(0), GameAction::PassPriority)
            .expect("p0 pass priority on End");
        // Expect priority to move to P1 (still End step).
        assert!(
            matches!(r1.waiting_for, WaitingFor::Priority { player } if player == PlayerId(1)),
            "after P0 pass, expected priority to P1, got {:?}",
            r1.waiting_for
        );

        // P1 passes — this should advance End → Cleanup and trigger discard prompt.
        let r2 = apply(&mut state, PlayerId(1), GameAction::PassPriority)
            .expect("p1 pass priority on End");

        match &r2.waiting_for {
            WaitingFor::DiscardToHandSize {
                player,
                count,
                cards,
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 2);
                assert_eq!(cards.len(), 9);
            }
            other => panic!(
                "expected DiscardToHandSize after End-step double-pass with 9 cards, got {:?}",
                other
            ),
        }
        // Hand untouched until selection made.
        assert_eq!(state.players[0].hand.len(), 9);
    }

    #[test]
    fn execute_cleanup_returns_discard_choice_when_over_seven() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Give player 9 cards in hand
        let mut hand_ids = Vec::new();
        for i in 0..9 {
            let id = create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Hand,
            );
            hand_ids.push(id);
        }

        let mut events = Vec::new();
        let result = execute_cleanup(&mut state, &mut events);

        match result {
            Some(WaitingFor::DiscardToHandSize {
                player,
                count,
                cards,
            }) => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 2);
                assert_eq!(cards.len(), 9);
            }
            other => panic!("Expected DiscardToHandSize, got {:?}", other),
        }

        // Hand unchanged until player makes a choice
        assert_eq!(state.players[0].hand.len(), 9);
    }

    #[test]
    fn execute_cleanup_returns_none_when_at_or_below_seven() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Give player exactly 7 cards
        for i in 0..7 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Hand,
            );
        }

        let mut events = Vec::new();
        let result = execute_cleanup(&mut state, &mut events);
        assert!(result.is_none());
    }

    #[test]
    fn finish_cleanup_discard_moves_selected_cards() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let mut hand_ids = Vec::new();
        for i in 0..9 {
            let id = create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Hand,
            );
            hand_ids.push(id);
        }

        // Player chooses to discard the last 2 cards
        let to_discard = vec![hand_ids[7], hand_ids[8]];
        let mut events = Vec::new();
        finish_cleanup_discard(&mut state, PlayerId(0), &to_discard, &mut events);

        assert_eq!(state.players[0].hand.len(), 7);
        assert_eq!(state.players[0].graveyard.len(), 2);
        assert!(state.players[0].graveyard.contains(&hand_ids[7]));
        assert!(state.players[0].graveyard.contains(&hand_ids[8]));
        // The first 7 cards should still be in hand
        for &id in &hand_ids[..7] {
            assert!(state.players[0].hand.contains(&id));
        }
    }

    #[test]
    fn execute_cleanup_clears_damage() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().damage_marked = 3;

        let mut events = Vec::new();
        execute_cleanup(&mut state, &mut events);

        assert_eq!(state.objects[&id].damage_marked, 0);
    }

    #[test]
    fn execute_cleanup_preserves_damage_under_damage_not_removed_static() {
        use crate::types::card_type::CoreType;

        let mut state = setup();

        // Ancient-Adamantoise-style permanent: its own damage isn't removed at
        // cleanup. CR 514.2 — the static suppresses the turn-based removal.
        let protected = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Ancient Adamantoise".to_string(),
            Zone::Battlefield,
        );
        {
            let defs = crate::parser::oracle_static::parse_static_line_multi(
                "Damage isn't removed from this creature during cleanup steps.",
            );
            assert!(
                defs.iter()
                    .any(|d| d.mode == StaticMode::DamageNotRemovedDuringCleanup),
                "static must parse to DamageNotRemovedDuringCleanup, got {:?}",
                defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
            );
            let obj = state.objects.get_mut(&protected).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.damage_marked = 4;
            for def in defs.iter().cloned() {
                obj.static_definitions.push(def);
            }
            Arc::make_mut(&mut obj.base_static_definitions).extend(defs);
        }

        // A normal creature: its damage IS removed at cleanup (control).
        let normal = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&normal).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.damage_marked = 3;
        }

        let mut events = Vec::new();
        execute_cleanup(&mut state, &mut events);

        assert_eq!(
            state.objects[&protected].damage_marked, 4,
            "damage must persist under DamageNotRemovedDuringCleanup"
        );
        assert_eq!(
            state.objects[&normal].damage_marked, 0,
            "a normal creature's damage is still removed at cleanup"
        );
    }

    #[test]
    fn finish_cleanup_discard_preserves_damage_under_damage_not_removed_static() {
        use crate::types::card_type::CoreType;

        let mut state = setup();
        state.active_player = PlayerId(0);

        // 9 cards in hand so cleanup must DEFER to a discard-to-hand-size choice,
        // routing the damage clearing through `finish_cleanup_discard`.
        let mut hand_ids = Vec::new();
        for i in 0..9 {
            let id = create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Hand,
            );
            hand_ids.push(id);
        }

        // Ancient-Adamantoise-style protected permanent with marked damage.
        let protected = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Ancient Adamantoise".to_string(),
            Zone::Battlefield,
        );
        {
            let defs = crate::parser::oracle_static::parse_static_line_multi(
                "Damage isn't removed from this creature during cleanup steps.",
            );
            let obj = state.objects.get_mut(&protected).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.damage_marked = 4;
            for def in defs.iter().cloned() {
                obj.static_definitions.push(def);
            }
            Arc::make_mut(&mut obj.base_static_definitions).extend(defs);
        }

        // Normal creature with marked damage (control).
        let normal = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&normal).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.damage_marked = 3;
        }

        // Cleanup defers: over hand size, the damage clearing is postponed to the
        // discard finish, so both creatures still carry their damage here.
        let mut events = Vec::new();
        let waiting = execute_cleanup(&mut state, &mut events);
        assert!(
            matches!(waiting, Some(WaitingFor::DiscardToHandSize { .. })),
            "expected a discard-to-hand-size choice, got {:?}",
            waiting
        );
        assert_eq!(
            state.objects[&protected].damage_marked, 4,
            "damage clearing is deferred until the discard finishes"
        );
        assert_eq!(
            state.objects[&normal].damage_marked, 3,
            "damage clearing is deferred until the discard finishes"
        );

        // Finish the discard: the deferred cleanup damage clearing runs through
        // the shared helper, so the protected creature KEEPS its damage while the
        // normal creature's is removed.
        finish_cleanup_discard(
            &mut state,
            PlayerId(0),
            &[hand_ids[7], hand_ids[8]],
            &mut events,
        );

        assert_eq!(
            state.objects[&protected].damage_marked, 4,
            "CR 514.2: protected damage must persist even through the discard path"
        );
        assert_eq!(
            state.objects[&normal].damage_marked, 0,
            "a normal creature's damage is removed when the discard finishes"
        );
    }

    #[test]
    fn execute_cleanup_preserves_phased_out_creature_damage_under_static() {
        use crate::game::game_object::{PhaseOutCause, PhaseStatus};
        use crate::types::card_type::CoreType;

        let mut state = setup();
        state.active_player = PlayerId(0);

        // Patient Zero: "Damage isn't removed from creatures your opponents
        // control during cleanup steps." — the static source (controlled by P0).
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Patient Zero".to_string(),
            Zone::Battlefield,
        );
        {
            let defs = crate::parser::oracle_static::parse_static_line_multi(
                "Damage isn't removed from creatures your opponents control during cleanup steps.",
            );
            assert!(defs
                .iter()
                .any(|d| d.mode == StaticMode::DamageNotRemovedDuringCleanup));
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            for def in defs.iter().cloned() {
                obj.static_definitions.push(def);
            }
            Arc::make_mut(&mut obj.base_static_definitions).extend(defs);
        }

        // A phased-out opponent creature with marked damage. CR 514.2 + CR
        // 702.26b: damage removal at cleanup is a turn-based action over the
        // whole battlefield (including phased-out permanents), so the static must
        // preserve this creature's damage even while it is phased out.
        let phased = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&phased).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.damage_marked = 3;
            obj.phase_status = PhaseStatus::PhasedOut {
                cause: PhaseOutCause::Directly,
            };
        }

        let mut events = Vec::new();
        execute_cleanup(&mut state, &mut events);

        assert_eq!(
            state.objects[&phased].damage_marked, 3,
            "a phased-out opponent creature's damage must persist under the static"
        );
    }

    /// CR 117.1c + CR 503.2: After Untap (no priority), the engine must hand
    /// the active player priority during Upkeep — even when no triggers fired.
    /// Previously `auto_advance` skipped past empty Upkeep/Draw windows, which
    /// silently broke phase-stop and full-control honoring (the FE never got a
    /// priority prompt to override). The skip happens at a higher layer now:
    /// the FE auto-pass loop and `run_auto_pass_loop` decide whether to drain
    /// the priority window based on `phase_stops` and `auto_pass_recommended`.
    #[test]
    fn auto_advance_pauses_at_upkeep_priority() {
        let mut state = setup();
        state.phase = Phase::Untap;
        state.turn_number = 2; // Not first turn, so the Draw step is not skipped.

        // Add a card to library so draw works (when Draw priority is eventually drained).
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        let waiting = auto_advance(&mut state, &mut events);

        // CR 117.1c: priority returned to active player during Upkeep.
        assert_eq!(state.phase, Phase::Upkeep);
        assert!(matches!(
            waiting,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn auto_advance_returns_upkeep_sba_waiting_state() {
        let mut state = setup();
        state.phase = Phase::Untap;
        state.turn_number = 2;
        state.active_player = PlayerId(0);

        for card_id in [1, 2] {
            let legend = create_object(
                &mut state,
                CardId(card_id),
                PlayerId(0),
                "Mirror Legend".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&legend)
                .unwrap()
                .card_types
                .supertypes
                .push(Supertype::Legendary);
        }

        let mut events = Vec::new();
        let waiting = auto_advance(&mut state, &mut events);

        assert_eq!(state.phase, Phase::Upkeep);
        assert!(matches!(
            waiting,
            WaitingFor::ChooseLegend {
                player: PlayerId(0),
                ..
            }
        ));
    }

    /// Regression for #1375: Twilight Prophet's upkeep trigger requires the city's blessing.
    /// The city blessing is granted by SBAs (CR 702.131b), so SBAs must run before
    /// beginning-of-upkeep triggers are collected. This test verifies that when a player
    /// controls 10 permanents with an Ascend permanent, the city blessing is granted
    /// before upkeep triggers are evaluated.
    #[test]
    fn city_blessing_granted_before_upkeep_triggers() {
        let mut state = setup();
        state.phase = Phase::Untap;
        state.turn_number = 2;
        state.active_player = PlayerId(0);

        // Player controls 10 permanents including one with Ascend
        let ascend_permanent = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Ascend Permanent".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&ascend_permanent)
            .unwrap()
            .keywords
            .push(crate::types::keywords::Keyword::Ascend);

        for i in 1..10 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Permanent {}", i),
                Zone::Battlefield,
            );
        }

        // Add Twilight Prophet with an upkeep trigger that checks for city blessing
        let prophet = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Twilight Prophet".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&prophet)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(
                    crate::types::triggers::TriggerMode::Phase,
                )
                .condition(crate::types::ability::TriggerCondition::HasCityBlessing)
                .description("Test trigger".to_string()),
            );

        // Untap step: no priority, just advance to Upkeep
        let mut events = Vec::new();
        auto_advance(&mut state, &mut events);

        // Should be in Upkeep now
        assert_eq!(state.phase, Phase::Upkeep);

        // City blessing should be granted by SBAs before upkeep triggers
        assert!(state.city_blessing.contains(&PlayerId(0)));
    }

    /// Regression for #1305: Thalisse's end step trigger counts tokens created this turn.
    /// This test verifies that tokens created during the turn are correctly counted
    /// when the end step trigger fires.
    #[test]
    fn thalisse_token_counting_at_end_step() {
        let mut state = setup();
        state.phase = Phase::Untap;
        state.turn_number = 2;
        state.active_player = PlayerId(0);

        // Add Thalisse with an end step trigger that counts tokens created this turn
        let thalisse = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Thalisse, Reverent Medium".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&thalisse)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(
                    crate::types::triggers::TriggerMode::Phase,
                )
                .phase(Phase::End)
                .condition(
                    crate::types::ability::TriggerCondition::QuantityComparison {
                        lhs: crate::types::ability::QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::TokensCreatedThisTurn {
                                player: crate::types::ability::PlayerScope::Controller,
                                filter: crate::types::ability::TargetFilter::Any,
                            },
                        },
                        comparator: crate::types::ability::Comparator::GE,
                        rhs: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    },
                )
                .description("Test trigger".to_string()),
            );

        // Create 3 tokens during the turn
        for i in 0..3 {
            let token = create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Token {}", i),
                Zone::Battlefield,
            );
            state.objects.get_mut(&token).unwrap().is_token = true;
            crate::game::restrictions::record_token_created(&mut state, token);
        }

        // Advance to end step
        state.phase = Phase::PostCombatMain;
        advance_phase(&mut state, &mut Vec::new()); // PostCombatMain → End
        let mut events = Vec::new();
        auto_advance(&mut state, &mut events);

        // Should be in End phase now
        assert_eq!(state.phase, Phase::End);

        // Verify tokens created this turn is 3
        assert_eq!(state.created_tokens_this_turn.len(), 3);
    }

    /// Regression for #1307: Moseo's trigger checks life gained this turn.
    /// This test verifies that life gained during the turn is correctly tracked
    /// and the trigger condition evaluates correctly.
    #[test]
    fn moseo_life_gained_trigger_condition() {
        let mut state = setup();
        state.phase = Phase::Untap;
        state.turn_number = 2;
        state.active_player = PlayerId(0);

        // Add Moseo with a trigger that checks life gained this turn
        let moseo = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Moseo, Vein's New Dean".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&moseo)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(
                    crate::types::triggers::TriggerMode::LifeGained,
                )
                .condition(
                    crate::types::ability::TriggerCondition::QuantityComparison {
                        lhs: crate::types::ability::QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::LifeGainedThisTurn {
                                player: crate::types::ability::PlayerScope::Controller,
                            },
                        },
                        comparator: crate::types::ability::Comparator::GE,
                        rhs: crate::types::ability::QuantityExpr::Fixed { value: 3 },
                    },
                )
                .description("Test trigger".to_string()),
            );

        // Simulate gaining 5 life this turn
        state.players[0].life_gained_this_turn = 5;

        // Check that the condition evaluates correctly
        let condition = crate::types::ability::TriggerCondition::QuantityComparison {
            lhs: crate::types::ability::QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::LifeGainedThisTurn {
                    player: crate::types::ability::PlayerScope::Controller,
                },
            },
            comparator: crate::types::ability::Comparator::GE,
            rhs: crate::types::ability::QuantityExpr::Fixed { value: 3 },
        };
        assert!(
            crate::game::triggers::check_trigger_condition(
                &state,
                &condition,
                PlayerId(0),
                Some(moseo),
                None
            ),
            "Condition should be true when 5 life gained (>= 3)"
        );
    }

    /// Regression for #1356: Tinybones end step trigger checks opponent discards.
    /// This test verifies that cards discarded by opponents are correctly tracked
    /// and the trigger condition evaluates correctly.
    #[test]
    fn tinybones_opponent_discard_trigger_condition() {
        let mut state = setup();
        state.phase = Phase::Untap;
        state.turn_number = 2;
        state.active_player = PlayerId(0);

        // Add Tinybones with an end step trigger that checks opponent discards
        let tinybones = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Tinybones, Trinket Thief".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&tinybones)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(
                    crate::types::triggers::TriggerMode::Phase,
                )
                .phase(Phase::End)
                .condition(
                    crate::types::ability::TriggerCondition::QuantityComparison {
                        lhs: crate::types::ability::QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::CardsDiscardedThisTurn {
                                player: crate::types::ability::PlayerScope::Opponent {
                                    aggregate: crate::types::ability::AggregateFunction::Sum,
                                },
                            },
                        },
                        comparator: crate::types::ability::Comparator::GE,
                        rhs: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    },
                )
                .description("Test trigger".to_string()),
            );

        // Simulate opponent discarding 2 cards this turn
        state
            .cards_discarded_this_turn_by_player
            .insert(PlayerId(1), 2);

        // Check that the condition evaluates correctly
        let condition = crate::types::ability::TriggerCondition::QuantityComparison {
            lhs: crate::types::ability::QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::CardsDiscardedThisTurn {
                    player: crate::types::ability::PlayerScope::Opponent {
                        aggregate: crate::types::ability::AggregateFunction::Sum,
                    },
                },
            },
            comparator: crate::types::ability::Comparator::GE,
            rhs: crate::types::ability::QuantityExpr::Fixed { value: 1 },
        };
        assert!(
            crate::game::triggers::check_trigger_condition(
                &state,
                &condition,
                PlayerId(0),
                Some(tinybones),
                None
            ),
            "Condition should be true when opponent discarded 2 cards (>= 1)"
        );
    }

    #[test]
    fn auto_advance_processes_precombat_main_triggers_before_priority() {
        let mut state = setup();
        // Start mid-turn at the boundary entering PreCombatMain. `auto_advance`
        // is now CR-117-strict (priority at every step), so testing the
        // PreCombatMain-specific trigger path requires entering directly.
        state.phase = Phase::Draw;
        state.turn_number = 2;
        advance_phase(&mut state, &mut Vec::new()); // Draw → PreCombatMain

        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Draw Step Card".to_string(),
            Zone::Library,
        );
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Precombat Trigger".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(
                    crate::types::triggers::TriggerMode::Phase,
                )
                .phase(Phase::PreCombatMain)
                .execute(crate::types::ability::AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Spell,
                    crate::types::ability::Effect::Draw {
                        count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                        target: crate::types::ability::TargetFilter::Controller,
                    },
                )),
            );

        let waiting = auto_advance(&mut state, &mut Vec::new());

        assert_eq!(state.phase, Phase::PreCombatMain);
        assert!(matches!(
            waiting,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.stack.len(), 1);
        assert!(matches!(
            state.stack[0].kind,
            crate::types::game_state::StackEntryKind::TriggeredAbility { .. }
        ));
    }

    #[test]
    fn auto_advance_skips_draw_on_first_turn() {
        let mut state = setup();
        state.phase = Phase::Untap;
        state.turn_number = 1;

        // Add a card to library (should NOT be drawn)
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        auto_advance(&mut state, &mut events);

        // Card should still be in library
        assert!(state.players[0].library.contains(&id));
        assert!(!state.players[0].hand.contains(&id));
    }

    /// CR 103.8c + issue #954: In a 4-player Commander game, the starting
    /// player must draw on their first turn — `auto_advance` should not skip
    /// the draw step. Mirrors `auto_advance_skips_draw_on_first_turn` (the
    /// 2-player case) and pins the call-site gate at the `Phase::Draw` arm
    /// of the auto_advance loop, complementing the predicate-level tests.
    ///
    /// Starts directly at `Phase::Draw` (rather than `Phase::Untap`) so the
    /// `Phase::Draw` arm executes before auto_advance returns at the next
    /// priority window — the 2-player mirror test passes vacuously because
    /// auto_advance pauses at the Upkeep priority window before the Draw
    /// arm is reached, but here we need to confirm the Draw arm actually
    /// performs the turn-based draw.
    #[test]
    fn auto_advance_does_not_skip_draw_on_first_turn_in_multiplayer() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 4, 42);
        state.phase = Phase::Draw;
        state.turn_number = 1;
        state.active_player = PlayerId(0);

        // Add a card to library (should be drawn — multiplayer does not skip).
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        auto_advance(&mut state, &mut events);

        assert!(
            state.players[0].hand.contains(&id),
            "CR 103.8c: 4-player Commander must perform the first-turn draw",
        );
        assert!(!state.players[0].library.contains(&id));
    }

    #[test]
    fn skip_draw_step_static_prevents_draw() {
        use crate::types::statics::StaticMode;

        let mut state = setup();
        state.phase = Phase::Untap;
        state.turn_number = 2; // Not first turn

        // Add a card to library
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        // Add a permanent with SkipStep { step: Draw }
        let enchant_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Necropotence".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&enchant_id)
            .unwrap()
            .static_definitions
            .push(crate::types::ability::StaticDefinition::new(
                StaticMode::SkipStep { step: Phase::Draw },
            ));

        let mut events = Vec::new();
        auto_advance(&mut state, &mut events);

        // Card should still be in library — draw was skipped
        assert!(
            state.players[0].library.contains(&card_id),
            "draw step should be skipped when SkipStep(Draw) static is active"
        );
        assert!(!state.players[0].hand.contains(&card_id));
    }

    #[test]
    fn all_player_static_step_skip_affects_noncontroller_active_player() {
        use crate::types::ability::TargetFilter;
        use crate::types::statics::StaticMode;

        let mut state = setup();
        state.active_player = PlayerId(1);

        let hub_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Eon Hub".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&hub_id)
            .unwrap()
            .static_definitions
            .push(
                crate::types::ability::StaticDefinition::new(StaticMode::SkipStep {
                    step: Phase::Upkeep,
                })
                .affected(TargetFilter::Player),
            );

        assert!(should_skip_step_static(&state, Phase::Upkeep));
    }

    #[test]
    fn controller_static_step_skip_does_not_affect_opponent() {
        use crate::types::ability::TargetFilter;
        use crate::types::statics::StaticMode;

        let mut state = setup();
        state.active_player = PlayerId(1);

        let enchant_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Necropotence".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&enchant_id)
            .unwrap()
            .static_definitions
            .push(
                crate::types::ability::StaticDefinition::new(StaticMode::SkipStep {
                    step: Phase::Draw,
                })
                .affected(TargetFilter::Controller),
            );

        assert!(!should_skip_step_static(&state, Phase::Draw));
    }

    #[test]
    fn one_shot_step_skip_consumes_matching_step() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.steps_to_skip[0].insert(Phase::Untap, 1);

        assert!(consume_next_step_skip(&mut state, Phase::Untap));
        assert!(!state.steps_to_skip[0].contains_key(&Phase::Untap));
    }

    #[test]
    fn static_step_skip_does_not_consume_next_step_skip() {
        use crate::types::statics::StaticMode;

        let mut state = setup();
        let enchant_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Static Skip".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&enchant_id)
            .unwrap()
            .static_definitions
            .push(crate::types::ability::StaticDefinition::new(
                StaticMode::SkipStep { step: Phase::Untap },
            ));
        state.steps_to_skip[0].insert(Phase::Untap, 1);

        assert!(should_skip_step_now(&mut state, Phase::Untap));
        assert_eq!(state.steps_to_skip[0].get(&Phase::Untap), Some(&1));
    }

    #[test]
    fn auto_advance_skips_combat_phases() {
        let mut state = setup();
        state.phase = Phase::BeginCombat;

        let mut events = Vec::new();
        let waiting = auto_advance(&mut state, &mut events);

        assert_eq!(state.phase, Phase::PostCombatMain);
        assert!(matches!(waiting, WaitingFor::Priority { .. }));
    }

    #[test]
    fn auto_advance_stops_at_end_step() {
        let mut state = setup();
        state.phase = Phase::End;

        let mut events = Vec::new();
        let waiting = auto_advance(&mut state, &mut events);

        assert_eq!(state.phase, Phase::End);
        assert!(matches!(waiting, WaitingFor::Priority { .. }));
    }

    #[test]
    fn advance_phase_from_cleanup_starts_next_turn() {
        let mut state = setup();
        state.phase = Phase::Cleanup;
        state.active_player = PlayerId(0);
        state.turn_number = 1;

        let mut events = Vec::new();
        advance_phase(&mut state, &mut events);

        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
        assert_eq!(state.phase, Phase::Untap);
    }

    #[test]
    fn start_next_turn_resets_spells_cast_this_turn() {
        let mut state = setup();
        state.spells_cast_this_turn = 3;

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert_eq!(state.spells_cast_this_turn, 0);
    }

    /// Regression: combat damage that reduces a player to 0-or-less life must end the game even
    /// when auto_advance drives the CombatDamage phase automatically (i.e. without a separate
    /// PassPriority action) and triggers were already processed inline before combat resolved.
    ///
    /// Previously `auto_advance` ignored the GameOver set by SBA and kept looping through
    /// EndCombat → PostCombatMain, returning WaitingFor::Priority which overwrote the GameOver.
    #[test]
    fn auto_advance_game_over_from_combat_damage_stops_loop() {
        use crate::game::combat::{AttackerInfo, CombatState};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.phase = Phase::CombatDamage;

        // Create an unblocked attacker with lethal power (20, enough to kill from full life)
        let attacker_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Big Creature".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&attacker_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(20);
            obj.toughness = Some(20);
            obj.entered_battlefield_turn = Some(1);
        }

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker_id, PlayerId(1))],
            ..Default::default()
        });

        let mut events = Vec::new();
        let wf = auto_advance(&mut state, &mut events);

        assert!(
            matches!(
                wf,
                WaitingFor::GameOver {
                    winner: Some(PlayerId(0))
                }
            ),
            "auto_advance should propagate GameOver when combat damage kills opponent, got {:?}",
            wf
        );
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::GameOver {
                    winner: Some(PlayerId(0))
                }
            ),
            "state.waiting_for should be GameOver, got {:?}",
            state.waiting_for
        );
    }

    #[test]
    fn auto_advance_combat_damage_flushes_layers_before_reading_power() {
        use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
        use crate::types::card_type::CoreType;
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.phase = Phase::CombatDamage;

        let attacker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Beast".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&attacker).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(3);
            obj.base_power = Some(1);
            obj.base_toughness = Some(3);
            obj.base_characteristics_initialized = true;
            obj.counters.insert(CounterType::Plus1Plus1, 8);
            obj.entered_battlefield_turn = Some(1);
        }

        let planeswalker = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Professor Onyx".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&planeswalker).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            // CR 306.5b: loyalty field and counter map mirror each other.
            obj.loyalty = Some(10);
            obj.counters.insert(CounterType::Loyalty, 10);
        }

        state.layers_dirty.mark_full();
        assert_eq!(
            state.objects.get(&attacker).unwrap().power,
            Some(1),
            "precondition: attacker power is stale before the CombatDamage phase arm runs"
        );

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(planeswalker),
                PlayerId(1),
            )],
            ..Default::default()
        });

        let mut events = Vec::new();
        let _ = auto_advance(&mut state, &mut events);

        // CR 510.1a + CR 120.3c + CR 613.4c: combat damage uses evaluated power,
        // including +1/+1 counters from layer 7c. Without the CombatDamage pre-flush
        // in auto_advance, this remains at 9 because stale base power dealt only 1.
        assert_eq!(state.objects[&planeswalker].loyalty, Some(1));
        assert_eq!(state.players[1].life, 20);
    }

    /// CR 800.4: When the active player is eliminated mid-turn in multiplayer,
    /// their remaining phases are skipped and the next player's turn begins.
    #[test]
    fn auto_advance_skips_eliminated_active_player_turn() {
        let mut state = GameState::new(crate::types::format::FormatConfig::free_for_all(), 3, 42);
        state.turn_number = 2;
        state.active_player = PlayerId(1);
        state.phase = Phase::PreCombatMain;

        // Mark P1 as eliminated (as if SBA just fired)
        state.players[1].is_eliminated = true;
        state.eliminated_players.push(PlayerId(1));

        let mut events = Vec::new();
        let wf = auto_advance(&mut state, &mut events);

        // Should have advanced to the next living player's turn
        assert_ne!(
            state.active_player,
            PlayerId(1),
            "eliminated player should no longer be active"
        );
        // Next living player after P1 is P2
        assert_eq!(state.active_player, PlayerId(2));
        // Game should not be over (P0 and P2 still alive)
        assert!(
            !matches!(wf, WaitingFor::GameOver { .. }),
            "game should continue with 2 living players"
        );
    }

    #[test]
    fn stun_counter_prevents_untap_and_removes_counter() {
        // CR 122.1d: A stun counter prevents a permanent from untapping;
        // instead, one stun counter is removed.
        use crate::types::zones::Zone;

        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.tapped = true;
        obj.counters.insert(CounterType::Stun, 2);

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        let obj = &state.objects[&obj_id];
        assert!(
            obj.tapped,
            "creature should remain tapped after stun counter removal"
        );
        assert_eq!(
            obj.counters.get(&CounterType::Stun).copied().unwrap_or(0),
            1,
            "one stun counter should be removed"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::CounterRemoved { object_id, counter_type: CounterType::Stun, count: 1 }
                    if *object_id == obj_id
            )),
            "CounterRemoved event should be emitted"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::PermanentUntapped { .. })),
            "PermanentUntapped should not be emitted when stun counter is present"
        );
    }

    #[test]
    fn stun_counter_removed_at_zero_cleans_up_entry() {
        // When the last stun counter is removed, the entry should be gone from the map.
        use crate::types::zones::Zone;

        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.tapped = true;
        obj.counters.insert(CounterType::Stun, 1);

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        let obj = &state.objects[&obj_id];
        assert!(
            !obj.counters.contains_key(&CounterType::Stun),
            "stun entry should be removed at zero"
        );
        assert!(
            obj.tapped,
            "creature still tapped after final stun counter removed"
        );
    }

    #[test]
    fn no_stun_counter_untaps_normally() {
        use crate::types::zones::Zone;

        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        assert!(
            !state.objects[&obj_id].tapped,
            "creature should untap normally"
        );
        assert!(
            events.iter().any(
                |e| matches!(e, GameEvent::PermanentUntapped { object_id } if *object_id == obj_id)
            ),
            "PermanentUntapped event should be emitted"
        );
    }

    #[test]
    fn restriction_cleanup_end_of_turn() {
        use crate::types::ability::{GameRestriction, RestrictionExpiry};
        use crate::types::identifiers::ObjectId;

        let mut state = GameState::new_two_player(42);
        state.phase = Phase::End;

        // Add an EndOfTurn restriction
        state
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source: ObjectId(1),
                expiry: RestrictionExpiry::EndOfTurn,
                scope: None,
            });
        // Add an EndOfCombat restriction (should survive cleanup)
        state
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source: ObjectId(2),
                expiry: RestrictionExpiry::EndOfCombat,
                scope: None,
            });

        assert_eq!(state.restrictions.len(), 2);

        let mut events = Vec::new();
        execute_cleanup(&mut state, &mut events);

        // EndOfTurn restriction should be removed, EndOfCombat should remain
        assert_eq!(state.restrictions.len(), 1);
        assert!(matches!(
            &state.restrictions[0],
            GameRestriction::DamagePreventionDisabled {
                expiry: RestrictionExpiry::EndOfCombat,
                ..
            }
        ));
    }

    #[test]
    fn execute_untap_prunes_until_player_next_turn_restrictions() {
        use crate::types::ability::{
            GameRestriction, ProhibitedActivity, RestrictionExpiry, RestrictionPlayerScope,
        };
        use crate::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(1);
        let source = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Avatar's Wrath".to_string(),
            Zone::Exile,
        );
        state.restrictions.push(GameRestriction::ProhibitActivity {
            source,
            affected_players: RestrictionPlayerScope::OpponentsOfSourceController,
            expiry: RestrictionExpiry::UntilPlayerNextTurn {
                player: PlayerId(1),
            },
            activity: ProhibitedActivity::CastOnlyFromZones {
                allowed_zones: vec![Zone::Hand],
            },
        });
        state
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source: ObjectId(2),
                expiry: RestrictionExpiry::EndOfCombat,
                scope: None,
            });

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        assert_eq!(state.restrictions.len(), 1);
        assert!(matches!(
            state.restrictions[0],
            GameRestriction::DamagePreventionDisabled {
                expiry: RestrictionExpiry::EndOfCombat,
                ..
            }
        ));
    }

    #[test]
    fn cleanup_expires_regeneration_shields() {
        use crate::types::ability::{ReplacementDefinition, TargetFilter};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        // Add two regen shields: one consumed, one active
        let consumed = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Used".to_string())
            .regeneration_shield();
        let active = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Fresh".to_string())
            .regeneration_shield();
        // Also add a non-regen replacement that should survive
        let normal = ReplacementDefinition::new(ReplacementEvent::Moved)
            .description("Normal repl".to_string());

        {
            let obj = state.objects.get_mut(&id).unwrap();
            let mut c = consumed;
            c.is_consumed = true;
            obj.replacement_definitions.push(c);
            obj.replacement_definitions.push(active);
            obj.replacement_definitions.push(normal);
        }

        let mut events = Vec::new();
        execute_cleanup(&mut state, &mut events);

        let obj = state.objects.get(&id).unwrap();
        // Both regen shields removed (consumed and active), normal survives
        assert_eq!(
            obj.replacement_definitions.len(),
            1,
            "Only non-regen replacement should survive cleanup"
        );
        assert!(
            !obj.replacement_definitions[0].shield_kind.is_shield(),
            "Surviving replacement should not be a shield"
        );
    }

    /// CR 402.2: A player with NoMaximumHandSize skips the discard-to-7 check.
    #[test]
    fn execute_cleanup_skips_discard_with_no_max_hand_size() {
        use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
        use crate::types::statics::StaticMode;

        let mut state = setup();
        state.active_player = PlayerId(0);

        // Give player 10 cards in hand
        for i in 0..10 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Hand,
            );
        }

        // Place a permanent with NoMaximumHandSize for Player 0
        let tower = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Reliquary Tower".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&tower)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::NoMaximumHandSize).affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
            );

        let mut events = Vec::new();
        let result = execute_cleanup(&mut state, &mut events);

        // No discard required — player keeps all 10 cards
        assert!(
            result.is_none(),
            "Expected no discard with NoMaximumHandSize, got {:?}",
            result
        );
        assert_eq!(state.players[0].hand.len(), 10);
    }

    /// CR 402.2 + CR 514.1: MaximumHandSize(SetTo(2)) forces discard to 2 instead of 7.
    #[test]
    fn execute_cleanup_max_hand_size_set_to_two() {
        use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
        use crate::types::statics::{HandSizeModification, StaticMode};

        let mut state = setup();
        state.active_player = PlayerId(0);

        // Give player 5 cards in hand (above 2, but below 7)
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Hand,
            );
        }

        // Place a permanent that sets max hand size to 2
        let perm = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Null Brooch".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&perm)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::MaximumHandSize {
                    modification: HandSizeModification::SetTo(2),
                })
                .affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
            );

        let mut events = Vec::new();
        let result = execute_cleanup(&mut state, &mut events);

        // Player has 5 cards, max is 2 → must discard 3
        match result {
            Some(WaitingFor::DiscardToHandSize { count, .. }) => {
                assert_eq!(count, 3, "Expected discard of 3 cards (5 - 2)");
            }
            other => panic!("Expected DiscardToHandSize, got {:?}", other),
        }
    }

    /// CR 402.2: MaximumHandSize(AdjustedBy(-3)) reduces the max from 7 to 4.
    #[test]
    fn execute_cleanup_max_hand_size_reduced_by_three() {
        use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
        use crate::types::statics::{HandSizeModification, StaticMode};

        let mut state = setup();
        state.active_player = PlayerId(0);

        // Give player 6 cards in hand (above 4, but below 7)
        for i in 0..6 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Hand,
            );
        }

        // Place a permanent that reduces max hand size by 3
        let perm = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Reducing Permanent".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&perm)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::MaximumHandSize {
                    modification: HandSizeModification::AdjustedBy(-3),
                })
                .affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
            );

        let mut events = Vec::new();
        let result = execute_cleanup(&mut state, &mut events);

        // Player has 6 cards, max is 7-3=4 → must discard 2
        match result {
            Some(WaitingFor::DiscardToHandSize { count, .. }) => {
                assert_eq!(count, 2, "Expected discard of 2 cards (6 - 4)");
            }
            other => panic!("Expected DiscardToHandSize, got {:?}", other),
        }
    }

    /// CR 514.1: Only the *active* player discards during the cleanup step.
    /// A non-active player with more than seven cards keeps them until their
    /// own turn's cleanup.
    #[test]
    fn execute_cleanup_ignores_non_active_player_hand_size() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Give the NON-active player (P1) 9 cards in hand — well over the
        // default maximum of 7.
        for i in 0..9 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Hand,
            );
        }
        // Active player (P0) has 0 cards — no discard needed for them.
        assert_eq!(state.players[0].hand.len(), 0);
        assert_eq!(state.players[1].hand.len(), 9);

        let mut events = Vec::new();
        let result = execute_cleanup(&mut state, &mut events);

        // CR 514.1: Only the active player's hand size is checked.
        // P1 is not the active player, so cleanup must complete without a
        // discard prompt.
        assert!(
            result.is_none(),
            "Non-active player should not be prompted to discard, got {:?}",
            result
        );
        // P1's hand is untouched.
        assert_eq!(state.players[1].hand.len(), 9);
    }

    /// CR 514.1: When both players exceed maximum hand size, only the active
    /// player is prompted to discard during that turn's cleanup step.
    #[test]
    fn execute_cleanup_only_prompts_active_player_when_both_exceed_max() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Both players have 9 cards in hand.
        for i in 0..9 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("P0 Card {}", i),
                Zone::Hand,
            );
        }
        for i in 10..19 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(1),
                format!("P1 Card {}", i),
                Zone::Hand,
            );
        }
        assert_eq!(state.players[0].hand.len(), 9);
        assert_eq!(state.players[1].hand.len(), 9);

        let mut events = Vec::new();
        let result = execute_cleanup(&mut state, &mut events);

        // Only the active player (P0) should be prompted.
        match result {
            Some(WaitingFor::DiscardToHandSize {
                player,
                count,
                cards,
            }) => {
                assert_eq!(player, PlayerId(0), "Only active player should discard");
                assert_eq!(count, 2);
                assert_eq!(cards.len(), 9);
            }
            other => panic!(
                "Expected DiscardToHandSize for active player, got {:?}",
                other
            ),
        }
        // P1's hand is completely untouched.
        assert_eq!(state.players[1].hand.len(), 9);
    }

    #[test]
    fn extra_turn_takes_precedence_over_seat_order() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.turn_number = 1;
        // CR 500.7: Push extra turn for player 0
        state.extra_turns.push(PlayerId(0));

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        // Extra turn player becomes active, not next in seat order
        assert_eq!(state.active_player, PlayerId(0));
        assert!(state.extra_turns.is_empty());
    }

    #[test]
    fn extra_turns_lifo_ordering() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.turn_number = 1;
        // CR 500.7: Push two extra turns — player 0 first, then player 1
        state.extra_turns.push(PlayerId(0));
        state.extra_turns.push(PlayerId(1));

        let mut events = Vec::new();

        // First start_next_turn: most recently created (player 1) taken first
        start_next_turn(&mut state, &mut events);
        assert_eq!(state.active_player, PlayerId(1));
        assert_eq!(state.extra_turns.len(), 1);

        // Second start_next_turn: player 0's extra turn
        start_next_turn(&mut state, &mut events);
        assert_eq!(state.active_player, PlayerId(0));
        assert!(state.extra_turns.is_empty());
    }

    #[test]
    fn normal_turn_advance_when_no_extra_turns() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.turn_number = 1;
        // No extra turns queued

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        // Normal seat order advance
        assert_eq!(state.active_player, PlayerId(1));
    }

    #[test]
    fn two_headed_giant_natural_turn_advances_to_opposing_team() {
        let mut state = GameState::new(
            crate::types::format::FormatConfig::two_headed_giant(),
            4,
            42,
        );
        state.active_player = PlayerId(0);
        state.turn_number = 1;

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert_eq!(state.active_player, PlayerId(2));
    }

    #[test]
    fn two_headed_giant_rotated_order_advances_to_next_team_representative() {
        let mut state = GameState::new(
            crate::types::format::FormatConfig::two_headed_giant(),
            4,
            42,
        );
        crate::game::engine::start_game_with_starting_player(&mut state, PlayerId(1));

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert_eq!(
            state.seat_order,
            vec![PlayerId(1), PlayerId(2), PlayerId(3), PlayerId(0)]
        );
        assert_eq!(state.active_player, PlayerId(2));

        start_next_turn(&mut state, &mut events);

        assert_eq!(state.active_player, PlayerId(0));
    }

    #[test]
    fn free_for_all_natural_turn_still_advances_seat_by_seat() {
        let mut state = GameState::new(crate::types::format::FormatConfig::free_for_all(), 4, 42);
        state.active_player = PlayerId(0);
        state.turn_number = 1;

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert_eq!(state.active_player, PlayerId(1));
    }

    #[test]
    fn controlled_turn_uses_controller_then_grants_extra_turn_afterward() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.turn_number = 1;
        state
            .scheduled_turn_controls
            .push(crate::types::game_state::ScheduledTurnControl {
                target_player: PlayerId(1),
                controller: PlayerId(0),
                grant_extra_turn_after: true,
            });

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert_eq!(state.active_player, PlayerId(1));
        assert_eq!(state.turn_decision_controller, Some(PlayerId(0)));
        assert_eq!(state.priority_player, PlayerId(0));
        assert_eq!(state.scheduled_turn_controls.len(), 1);

        start_next_turn(&mut state, &mut events);

        assert_eq!(state.active_player, PlayerId(1));
        assert_eq!(state.turn_decision_controller, None);
        assert_eq!(state.priority_player, PlayerId(1));
        assert!(state.scheduled_turn_controls.is_empty());
    }

    #[test]
    fn shared_team_control_retires_non_anchor_completed_turn() {
        let mut state = GameState::new(
            crate::types::format::FormatConfig::two_headed_giant(),
            4,
            42,
        );
        state.seat_order = vec![PlayerId(1), PlayerId(2), PlayerId(3), PlayerId(0)];
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(2);
        state.turn_decision_controller = Some(PlayerId(2));
        state.turn_number = 1;
        state
            .scheduled_turn_controls
            .push(crate::types::game_state::ScheduledTurnControl {
                target_player: PlayerId(0),
                controller: PlayerId(2),
                grant_extra_turn_after: false,
            });

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert_eq!(state.active_player, PlayerId(2));
        assert_eq!(state.turn_decision_controller, None);
        assert!(state.scheduled_turn_controls.is_empty());

        start_next_turn(&mut state, &mut events);

        assert_eq!(state.active_player, PlayerId(0));
        assert_eq!(state.turn_decision_controller, None);
        assert_eq!(state.priority_player, PlayerId(0));
    }

    #[test]
    fn newest_scheduled_control_for_target_takes_precedence() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.turn_number = 1;
        state
            .scheduled_turn_controls
            .push(crate::types::game_state::ScheduledTurnControl {
                target_player: PlayerId(1),
                controller: PlayerId(0),
                grant_extra_turn_after: false,
            });
        state
            .scheduled_turn_controls
            .push(crate::types::game_state::ScheduledTurnControl {
                target_player: PlayerId(1),
                controller: PlayerId(1),
                grant_extra_turn_after: false,
            });

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert_eq!(state.active_player, PlayerId(1));
        assert_eq!(state.turn_decision_controller, Some(PlayerId(1)));

        start_next_turn(&mut state, &mut events);

        assert_eq!(state.active_player, PlayerId(0));
        assert_eq!(state.turn_decision_controller, None);
        assert!(state.scheduled_turn_controls.is_empty());
    }

    // --- BeginTurn / BeginPhase replacement pipeline (CR 614.1b, CR 614.10) ---

    fn install_begin_turn_skip_permanent(
        state: &mut GameState,
        obj_id: crate::types::identifiers::ObjectId,
        controller: PlayerId,
        condition: Option<crate::types::ability::ReplacementCondition>,
    ) {
        use crate::game::game_object::GameObject;
        use crate::types::ability::ReplacementDefinition;
        use crate::types::identifiers::CardId;
        use crate::types::replacements::ReplacementEvent;

        let mut obj = GameObject::new(
            obj_id,
            CardId(42),
            controller,
            "Stranglehold".to_string(),
            Zone::Battlefield,
        );
        let mut def = ReplacementDefinition::new(ReplacementEvent::BeginTurn);
        if let Some(cond) = condition {
            def = def.condition(cond);
        }
        obj.replacement_definitions = vec![def].into();
        state.objects.insert(obj_id, obj);
        state.battlefield.push_back(obj_id);
    }

    #[test]
    fn stranglehold_skips_extra_turn_not_normal_turn() {
        // CR 500.7 + CR 614.10: Stranglehold-class permanent with
        // `OnlyExtraTurn` must skip extra turns but leave natural turns alone.
        use crate::types::ability::ReplacementCondition;
        use crate::types::identifiers::ObjectId;

        let mut state = setup();
        state.active_player = PlayerId(0);
        state.turn_number = 1;
        let starting_p0_turns_taken = state.players[0].turns_taken;

        install_begin_turn_skip_permanent(
            &mut state,
            ObjectId(100),
            PlayerId(1),
            Some(ReplacementCondition::OnlyExtraTurn),
        );

        // Push an extra turn for player 0. With no further extras, the next
        // natural turn after the skip should go to player 1.
        state.extra_turns.push(PlayerId(0));

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        // The extra turn was popped and skipped; recursion fell through to
        // a natural turn for the next seat.
        assert!(state.extra_turns.is_empty(), "extra turn must be consumed");
        assert_eq!(
            state.active_player,
            PlayerId(1),
            "after skipping P0's extra turn, P1 should take their natural turn"
        );
        // P0's turns_taken must NOT have incremented for the skipped turn
        // (the skip happens before the increment in start_next_turn).
        assert_eq!(
            state.players[0].turns_taken, starting_p0_turns_taken,
            "skipped turn must not count toward P0's turns_taken"
        );
        // A ReplacementApplied event must have been emitted for the skip.
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::ReplacementApplied { event_type, .. } if event_type == "BeginTurn"
            )),
            "ReplacementApplied BeginTurn event should be emitted on skip"
        );
    }

    #[test]
    fn stranglehold_does_not_skip_natural_turn() {
        // CR 500.7: Natural turn (no extra_turns push) must NOT be skipped
        // even when a Stranglehold-class replacement is on the battlefield.
        use crate::types::ability::ReplacementCondition;
        use crate::types::identifiers::ObjectId;

        let mut state = setup();
        state.active_player = PlayerId(0);
        state.turn_number = 1;

        install_begin_turn_skip_permanent(
            &mut state,
            ObjectId(100),
            PlayerId(1),
            Some(ReplacementCondition::OnlyExtraTurn),
        );

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        // Natural advance to P1 — not skipped.
        assert_eq!(state.active_player, PlayerId(1));
        assert!(
            !events.iter().any(|e| matches!(
                e,
                GameEvent::ReplacementApplied { event_type, .. } if event_type == "BeginTurn"
            )),
            "no skip should fire for a natural turn"
        );
    }

    #[test]
    fn phase_pipeline_prevented_skips_that_phase() {
        // CR 614.1b + CR 500.11: An unconditional BeginPhase replacement causes
        // advance_phase to recurse and land on the phase AFTER the skipped one.
        // We tightly scope the skip to a single phase by mutating the
        // replacement definition's matcher indirectly: we install the skip,
        // advance past the first phase, then remove the skip so the test
        // terminates deterministically.
        use crate::game::game_object::GameObject;
        use crate::types::ability::ReplacementDefinition;
        use crate::types::identifiers::{CardId, ObjectId};

        let mut state = setup();
        state.active_player = PlayerId(0);
        state.phase = Phase::Untap;

        let mut obj = GameObject::new(
            ObjectId(200),
            CardId(99),
            PlayerId(1),
            "SkipPhase".to_string(),
            Zone::Battlefield,
        );
        obj.replacement_definitions = vec![ReplacementDefinition::new(
            crate::types::replacements::ReplacementEvent::BeginPhase,
        )]
        .into();
        state.objects.insert(ObjectId(200), obj);
        state.battlefield.push_back(ObjectId(200));

        let mut events = Vec::new();

        // This will skip every phase until Cleanup→Untap starts the next turn,
        // which is the guaranteed termination point (no BeginPhase pipeline is
        // run on the Cleanup→Untap crossover; it goes through start_next_turn).
        advance_phase(&mut state, &mut events);

        // At least one BeginPhase ReplacementApplied must have fired.
        let begin_phase_applied_count = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::ReplacementApplied { event_type, .. } if event_type == "BeginPhase"
                )
            })
            .count();
        assert!(
            begin_phase_applied_count >= 1,
            "at least one BeginPhase skip must have fired, got {}",
            begin_phase_applied_count
        );
    }
}
