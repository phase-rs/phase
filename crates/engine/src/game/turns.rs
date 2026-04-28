use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{AutoPassMode, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::statics::{HandSizeModification, StaticMode};
use crate::types::zones::Zone;

use super::combat;
use super::combat_damage;
use super::day_night;
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

/// CR 500.4: Advance to the next phase/step, clearing mana pools.
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

    state.phase = next;

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
    for player in &mut state.players {
        player.mana_pool.clear_step_transition(in_combat);
        // CR 121.1 + CR 504.1: `cards_drawn_this_step` resets on every step
        // transition so `ExceptFirstDrawInDrawStep` conditions can identify
        // the first card drawn during the new step (most importantly, the
        // draw step's mandatory turn-based draw).
        player.cards_drawn_this_step = 0;
    }

    // CR 117.3a: Active player receives priority at the beginning of most steps and phases.
    state.priority_player = turn_control::turn_decision_maker(state);
    state.priority_passes.clear();
    state.priority_pass_count = 0;
    state.players_attacked_this_step.clear();
    // CR 400.7: LKI persists within a step but is invalidated on step transition.
    state.lki_cache.clear();

    events.push(GameEvent::PhaseChanged { phase: next });
}

/// Begin the next player's turn (CR 500.1 / CR 101.4 seat order).
pub fn start_next_turn(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let completed_player = state.active_player;
    if state.turn_decision_controller.is_some() {
        let mut grant_extra_turn_after = false;
        state.scheduled_turn_controls.retain(|scheduled| {
            if scheduled.target_player != completed_player {
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
        state.active_player = extra_turn_player;
        true
    } else {
        state.active_player = super::players::next_player(state, state.active_player);
        false
    };

    // CR 614.10: Simple turn-skip counter (effect-based, e.g., Meditate, Eater of
    // Days). This is a fast path for "you skip your next turn" that doesn't need
    // the replacement pipeline — there's no event-context predicate to evaluate.
    let idx = state.active_player.0 as usize;
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

    if let Some(scheduled) = state
        .scheduled_turn_controls
        .iter()
        .rfind(|scheduled| scheduled.target_player == state.active_player)
        .copied()
    {
        state.turn_decision_controller = Some(scheduled.controller);
    }

    // Reset priority
    state.priority_player = turn_control::turn_decision_maker(state);
    state.priority_passes.clear();
    state.priority_pass_count = 0;

    // Reset per-turn counters
    // CR 305.2: Reset per-turn land play count.
    state.lands_played_this_turn = 0;
    // CR 603.4: Snapshot spell count for werewolf "last turn" conditions before resetting.
    state.spells_cast_last_turn = Some(state.spells_cast_this_turn);
    // CR 500.1: Reset per-turn spell cast counters.
    state.spells_cast_this_turn = 0;
    state.triggers_fired_this_turn.clear();
    state.trigger_fire_counts_this_turn.clear();
    state.activated_abilities_this_turn.clear();
    // CR 514 + CR 603.4: Per-ability per-turn resolution counter resets at turn
    // boundary alongside other "this turn" trackers (mirrors the cleanup of
    // `trigger_fire_counts_this_turn`).
    state.ability_resolutions_this_turn.clear();
    state.graveyard_cast_permissions_used.clear();
    // CR 601.2b: Reset per-turn CastFromHandFree once-per-turn tracking (Zaffai).
    state.hand_cast_free_permissions_used.clear();
    // CR 702.94a: Reset per-player first-card-drawn-this-turn tracking for miracle.
    state.first_card_drawn_this_turn.clear();
    // CR 702.94a: Any miracle offers that outlived priority without being
    // flushed are stale (the "first card drawn this turn" condition no longer
    // applies after the turn ends). Drop them so we never surface a prompt for
    // a card drawn last turn.
    state.pending_miracle_offers.clear();
    state.spells_cast_this_turn_by_player.clear();
    state.players_who_searched_library_this_turn.clear();
    state.players_attacked_this_step.clear();
    state.players_attacked_this_turn.clear();
    state.attacking_creatures_this_turn.clear();
    state.creatures_attacked_this_turn.clear();
    state.creatures_blocked_this_turn.clear();
    state.players_who_created_token_this_turn.clear();
    state.players_who_added_counter_this_turn.clear();
    state.players_who_discarded_card_this_turn.clear();
    state.players_who_sacrificed_artifact_this_turn.clear();
    state.zone_changes_this_turn.clear();
    state.battlefield_entries_this_turn.clear();
    state.damage_dealt_this_turn.clear();
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

    // CR 302.6: At the start of a player's turn, any permanent they have
    // controlled continuously since before this moment has now been under
    // their control "since that player's most recent turn began" — clear
    // summoning sickness. CR 606.3: Loyalty abilities may be activated only
    // once per turn per permanent — reset the per-turn flag in the same pass.
    let active = state.active_player;
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        if obj.controller == active {
            if obj.summoning_sick {
                obj.summoning_sick = false;
            }
            if obj.loyalty_activated_this_turn {
                obj.loyalty_activated_this_turn = false;
            }
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
    state.restrictions.retain(|restriction| {
        use crate::types::ability::{GameRestriction, RestrictionExpiry};

        match restriction {
            GameRestriction::CastOnlyFromZones { expiry, .. }
            | GameRestriction::CantCastSpells { expiry, .. } => {
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

    // CR 302.6: Also check intrinsic CantUntap statics on objects
    // (permanent "doesn't untap" from auras/enchantments).
    let intrinsic_cant_untap: HashSet<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                // CR 702.26b + CR 604.1: `active_static_definitions` owns the gating.
                obj.controller == active
                    && super::functioning_abilities::active_static_definitions(state, obj).any(
                        |sd| {
                            sd.mode == StaticMode::CantUntap
                                && super::static_abilities::check_static_ability(
                                    state,
                                    StaticMode::CantUntap,
                                    &super::static_abilities::StaticCheckContext {
                                        target_id: Some(*id),
                                        ..Default::default()
                                    },
                                )
                        },
                    )
            })
        })
        .collect();

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
        // CR 502.3: Skip permanents that have CantUntap (transient or intrinsic).
        if cant_untap_ids.contains(&id) || intrinsic_cant_untap.contains(&id) {
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
pub fn execute_draw(state: &mut GameState, events: &mut Vec<GameEvent>) -> Option<WaitingFor> {
    let active = state.active_player;

    // CR 121.1 + CR 614.1a: Route through replacement pipeline (Dredge, Abundance, etc.).
    let proposed = ProposedEvent::Draw {
        player_id: active,
        count: 1,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::Draw {
                player_id, count, ..
            } = event
            {
                let allowed =
                    crate::game::effects::draw::allowed_draw_count(state, player_id, count);

                let cards_to_draw: Vec<_> = state
                    .players
                    .iter()
                    .find(|p| p.id == player_id)
                    .map(|p| p.library.iter().take(allowed as usize).copied().collect())
                    .unwrap_or_default();

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
                    let nth_in_step =
                        if let Some(p) = state.players.iter_mut().find(|p| p.id == player_id) {
                            p.has_drawn_this_turn = true;
                            p.cards_drawn_this_turn = p.cards_drawn_this_turn.saturating_add(1);
                            p.cards_drawn_this_step = p.cards_drawn_this_step.saturating_add(1);
                            p.cards_drawn_this_step
                        } else {
                            1
                        };
                    // CR 121.1: Emit CardDrawn so "whenever a player draws" triggers fire.
                    events.push(GameEvent::CardDrawn {
                        player_id,
                        object_id: obj_id,
                        nth_in_step,
                    });
                }
            }
        }
        ReplacementResult::Prevented => {
            // Draw was prevented (e.g., "can't draw cards" effect)
        }
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Some(state.waiting_for.clone());
        }
    }

    None
}

/// Execute the cleanup step. Returns `Some(WaitingFor)` if the player must
/// choose which cards to discard down to maximum hand size, or `None` if
/// cleanup completes immediately.
pub fn execute_cleanup(state: &mut GameState, events: &mut Vec<GameEvent>) -> Option<WaitingFor> {
    // CR 701.19b: Regeneration shields expire at cleanup.
    // CR 615: Prevention effects also expire.
    // CR 514.2: Resolution-time replacements with `expires_at_eot` (e.g., the
    // "if [target] would die this turn, exile it instead" rider on damage
    // spells) also expire here regardless of whether they fired.
    // Also prune any consumed shields from earlier this turn.
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        obj.replacement_definitions
            .retain(|r| !(r.shield_kind.is_shield() || r.expires_at_eot));
    }
    // CR 615.3: Clear game-state-level prevention shields (fog-like spells).
    state.pending_damage_prevention.clear();

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
            | GameRestriction::CastOnlyFromZones { expiry, .. }
            | GameRestriction::CantCastSpells { expiry, .. } => {
                !matches!(expiry, RestrictionExpiry::EndOfTurn)
            }
        }
    });

    // CR 603.7b + CR 603.7c: Remove "this turn" delayed triggers at cleanup.
    // WheneverEvent (multi-fire, one_shot=false) triggers persist until cleanup.
    // WhenNextEvent (one-shot) triggers that didn't fire also expire — their
    // "this turn" duration means they must not carry over to the next turn.
    state.delayed_triggers.retain(|dt| {
        dt.one_shot
            && !matches!(
                dt.condition,
                crate::types::ability::DelayedTriggerCondition::WhenNextEvent { .. }
            )
    });

    // CR 730.2: Check day/night transition at cleanup.
    day_night::check_day_night_transition(state, events);

    let active = state.active_player;

    // CR 514.1 + CR 402.2: Discard down to maximum hand size.
    // If the player has "no maximum hand size" (CR 402.2), skip the discard check entirely.
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

    // CR 514.2: Damage on creatures is removed at cleanup.
    let to_clear: Vec<_> = state
        .battlefield
        .iter()
        .copied()
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

    // CR 702.171b: "Once a permanent has become saddled, it stays saddled until
    // the end of the turn or it leaves the battlefield." Clear the designation
    // at cleanup (CR 514).
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        if obj.is_saddled {
            obj.is_saddled = false;
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

    // Clear damage on all battlefield creatures (deferred from execute_cleanup)
    let to_clear: Vec<_> = state
        .battlefield
        .iter()
        .copied()
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
    false
}

/// CR 103.8a: The player who goes first skips their first draw step.
/// CR 614.1b + CR 614.10: Also skip if a "skip your draw step" static is active.
pub fn should_skip_draw(state: &GameState) -> bool {
    state.turn_number == 1 || should_skip_step(state, Phase::Draw)
}

/// CR 614.1b + CR 614.10: Check whether the active player should skip the given step
/// due to a "skip your [step] step" static ability on a permanent they control.
fn should_skip_step(state: &GameState, step: Phase) -> bool {
    let active = state.active_player;
    // CR 702.26b + CR 604.1: `active_static_definitions` owns the gating.
    state.battlefield.iter().any(|id| {
        state.objects.get(id).is_some_and(|obj| {
            obj.controller == active
                && super::functioning_abilities::active_static_definitions(state, obj)
                    .any(|sd| sd.mode == StaticMode::SkipStep { step })
        })
    })
}

/// CR 714.3b: As the precombat main phase begins, put a lore counter on each Saga
/// the active player controls. This is a turn-based action, not a triggered ability.
fn add_lore_counters_to_sagas(state: &mut GameState, events: &mut Vec<GameEvent>) {
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
    for saga_id in saga_ids {
        super::effects::counters::add_counter_with_replacement(
            state,
            saga_id,
            CounterType::Lore,
            1,
            events,
        );
    }
}

/// CR 503.1 / CR 504.2 / CR 507.1 / CR 513.1: Process phase triggers for the current step.
/// Fabricates a PhaseChanged event for `state.phase` and runs trigger matching.
/// Returns `true` if any triggers were placed on the stack or are pending target selection.
fn process_phase_triggers(state: &mut GameState) -> bool {
    let phase_event = [GameEvent::PhaseChanged { phase: state.phase }];
    let stack_before = state.stack.len();
    super::triggers::process_triggers(state, &phase_event);
    state.stack.len() > stack_before || state.pending_trigger.is_some()
}

pub fn auto_advance(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    loop {
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return state.waiting_for.clone();
        }

        match state.phase {
            Phase::Untap => {
                // CR 614.1b: Skip the untap step if a "skip your untap step" static is active.
                if !should_skip_step(state, Phase::Untap) {
                    execute_untap(state, events);
                }
                // CR 502.4 / CR 117.3a: No player receives priority during the untap step.
                advance_phase(state, events);
            }
            Phase::Upkeep => {
                // CR 503.1a: "At the beginning of [your] upkeep" triggers fire here.
                if process_phase_triggers(state) {
                    return WaitingFor::Priority {
                        player: state.active_player,
                    };
                }
                advance_phase(state, events);
            }
            Phase::Draw => {
                if !should_skip_draw(state) {
                    if let Some(wf) = execute_draw(state, events) {
                        return wf;
                    }
                }
                // CR 504.2: "At the beginning of [your] draw step" triggers fire here.
                if process_phase_triggers(state) {
                    return WaitingFor::Priority {
                        player: state.active_player,
                    };
                }
                advance_phase(state, events);
            }
            Phase::PreCombatMain | Phase::PostCombatMain => {
                // CR 714.3b: As the precombat main phase begins, add a lore counter
                // to each Saga the active player controls (turn-based action).
                if state.phase == Phase::PreCombatMain {
                    add_lore_counters_to_sagas(state, events);
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
                // CR 505.1: The active player receives priority at the start of their main phase.
                return WaitingFor::Priority {
                    player: state.active_player,
                };
            }
            Phase::BeginCombat => {
                // CR 507.1: "At the beginning of combat" triggers fire here.
                // Process triggers regardless of attackers — CR 507.1 says the step
                // happens unconditionally; trigger conditions (e.g., ControlCreatures)
                // are checked by the trigger system, not by skipping the step.
                let triggers_fired = process_phase_triggers(state);
                if triggers_fired {
                    state.combat = Some(crate::game::combat::CombatState::default());
                    return WaitingFor::Priority {
                        player: state.active_player,
                    };
                }
                if combat::has_potential_attackers(state) {
                    state.combat = Some(crate::game::combat::CombatState::default());
                    advance_phase(state, events);
                    // Continue to DeclareAttackers
                } else {
                    // No triggers, no attackers — skip all combat phases.
                    state.combat = None;
                    state.phase = Phase::PostCombatMain;
                    state.priority_player = state.active_player;
                    state.priority_passes.clear();
                    state.priority_pass_count = 0;
                    events.push(GameEvent::PhaseChanged {
                        phase: Phase::PostCombatMain,
                    });
                    return WaitingFor::Priority {
                        player: state.active_player,
                    };
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
                let has_attackers = state
                    .combat
                    .as_ref()
                    .is_some_and(|c| !c.attackers.is_empty());
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
                    return WaitingFor::DeclareBlockers {
                        player: defending,
                        valid_blocker_ids,
                        valid_block_targets,
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
                // CR 510.1 / CR 510.2: Combat damage assigned and dealt as a turn-based action.
                // resolve_combat_damage may pause for interactive assignment (2+ blockers).
                if let Some(waiting) = combat_damage::resolve_combat_damage(state, events) {
                    state.waiting_for = waiting.clone();
                    return waiting;
                }
                // CR 704.3 / CR 800.4: SBAs may have ended the game during combat damage.
                if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
                    return state.waiting_for.clone();
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
                let triggers_fired = process_phase_triggers(state);
                // CR 511.3: At end of combat, all creatures are removed from combat.
                state.combat = None;
                super::layers::prune_end_of_combat_effects(state);
                if triggers_fired {
                    return WaitingFor::Priority {
                        player: state.active_player,
                    };
                }
                advance_phase(state, events);
                // Continue to PostCombatMain
            }
            Phase::End => {
                // CR 513.1: End step — active player receives priority.
                // CR 513.1a: "At the beginning of [your] end step" triggers fire here.
                process_phase_triggers(state);
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
    use crate::types::identifiers::CardId;
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
        state.combat = Some(combat::CombatState {
            attackers: vec![combat::AttackerInfo::new(
                ObjectId(1),
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
    fn advance_phase_clears_mana_pools() {
        use crate::types::identifiers::ObjectId;
        use crate::types::mana::{ManaType, ManaUnit};

        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(1),
            snow: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });

        let mut events = Vec::new();
        advance_phase(&mut state, &mut events);

        assert_eq!(state.players[0].mana_pool.total(), 0);
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
        // Mirror `additional_combat::resolve` push order with `with_main_phase = true`.
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

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert_eq!(state.lands_played_this_turn, 0);
        assert!(!state.players[0].has_drawn_this_turn);
        assert_eq!(state.players[0].lands_played_this_turn, 0);
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

    #[test]
    fn should_skip_draw_on_turn_1() {
        let mut state = setup();
        state.turn_number = 1;
        assert!(should_skip_draw(&state));

        state.turn_number = 2;
        assert!(!should_skip_draw(&state));
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
    fn auto_advance_skips_to_precombat_main() {
        let mut state = setup();
        state.phase = Phase::Untap;
        state.turn_number = 2; // Not first turn, so draw happens

        // Add a card to library so draw works
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        let waiting = auto_advance(&mut state, &mut events);

        assert_eq!(state.phase, Phase::PreCombatMain);
        assert!(matches!(
            waiting,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
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
        use crate::types::ability::{GameRestriction, RestrictionExpiry, RestrictionPlayerScope};
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
        state.restrictions.push(GameRestriction::CastOnlyFromZones {
            source,
            affected_players: RestrictionPlayerScope::OpponentsOfSourceController,
            allowed_zones: vec![Zone::Hand],
            expiry: RestrictionExpiry::UntilPlayerNextTurn {
                player: PlayerId(1),
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
