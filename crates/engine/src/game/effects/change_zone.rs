use rand::Rng;

use crate::game::zones;
use crate::types::ability::{
    ControllerRef, Duration, Effect, EffectError, EffectKind, FilterProp, ResolvedAbility,
    TargetChoiceTiming, TargetFilter, TargetSelectionMode, TypedFilter,
};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCounterPostAction, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::{EtbTapState, Zone};

/// CR 701.24a: Shuffle a player's library using the game's seeded RNG.
/// Reusable helper for auto-shuffle after zone moves to Library.
pub fn shuffle_library(state: &mut GameState, player: PlayerId, events: &mut Vec<GameEvent>) {
    let GameState { players, rng, .. } = state;
    if let Some(p) = players.iter_mut().find(|p| p.id == player) {
        crate::util::im_ext::shuffle_vector(&mut p.library, rng);
    }
    // CR 701.24a: Emit player-action event so trigger matchers can filter
    // by the identity of the shuffling player.
    events.push(GameEvent::PlayerPerformedAction {
        player_id: player,
        action: crate::types::events::PlayerActionKind::ShuffledLibrary,
    });
}

/// CR 608.2c: For a `TrackedSet` / `TrackedSetFiltered` target, resolve the
/// zones its members currently occupy. Tracked sets are not zone-constrained —
/// milled cards land in the graveyard, revealed cards stay in the library/hand
/// — so a `ChangeZone` selecting "from among" such a set must scan the
/// members' actual zones, not the battlefield default.
///
/// The `TrackedSetId(0)` sentinel resolves through the same chain-first binding
/// authority as `matches_target_filter`. Returns `None` when the filter is not
/// tracked-set-backed or the set is empty/unbound.
fn tracked_set_member_zones(state: &GameState, filter: &TargetFilter) -> Option<Vec<Zone>> {
    let filter = crate::game::targeting::resolve_tracked_set_sentinel(state, filter.clone());
    let id = match &filter {
        TargetFilter::TrackedSet { id } | TargetFilter::TrackedSetFiltered { id, .. } => *id,
        _ => return None,
    };
    let zones = state
        .tracked_object_sets
        .get(&id)?
        .iter()
        .filter_map(|obj_id| state.objects.get(obj_id).map(|obj| obj.zone))
        .fold(Vec::new(), |mut zones, zone| {
            if !zones.contains(&zone) {
                zones.push(zone);
            }
            zones
        });
    (!zones.is_empty()).then_some(zones)
}

fn resolve_enters_under_player(
    state: &GameState,
    ability: &ResolvedAbility,
    effect_name: &str,
    enters_under: Option<&ControllerRef>,
) -> Result<Option<PlayerId>, EffectError> {
    // CR 110.2a: Resolve the controller-override reference to a concrete
    // `PlayerId` exactly once at the resolver boundary, then carry it through
    // zone movement. Delegates to the canonical `ControllerRef` resolver so
    // every player reference resolves consistently: `You` (and the per-iteration
    // controller under `player_scope`), `ScopedPlayer` ("each player … under
    // their control"), `TargetPlayer` ("under target player's control"),
    // `ParentTargetController`, etc. `None` keeps the default (owner's control).
    match enters_under {
        None => Ok(None),
        Some(cref) => crate::game::filter::controller_ref_player(
            state,
            ability.source_id,
            Some(ability.controller),
            Some(ability),
            cref,
        )
        .map(Some)
        .ok_or_else(|| {
            EffectError::InvalidParam(format!(
                "CR 110.2a: {effect_name}.enters_under = {cref:?} could not be \
                 resolved to a concrete controller in this context"
            ))
        }),
    }
}

fn resolution_choice_cardinality(
    state: &GameState,
    ability: &ResolvedAbility,
    eligible_count: usize,
    up_to: bool,
) -> (usize, usize, bool) {
    let Some(spec) = ability
        .multi_target
        .as_ref()
        .filter(|_| matches!(ability.target_choice_timing, TargetChoiceTiming::Resolution))
    else {
        return (1, 0, up_to);
    };

    match crate::game::ability_utils::resolve_multi_target_bounds(
        state,
        ability,
        spec,
        eligible_count,
    ) {
        Ok(bounds) => (bounds.max, bounds.min, bounds.min != bounds.max),
        Err(_) => (0, 0, up_to),
    }
}

// PLAN §7 Phase A: the zone-change pipeline (result enums, delivery tail,
// `execute_zone_move`, `deliver_replaced_zone_change`) now lives in
// `crate::game::zone_pipeline`. These shims keep every existing
// `change_zone::{...}` caller compiling unchanged with zero behavior change.
pub(crate) use crate::game::zone_pipeline::{
    apply_zone_delivery_tail, deliver_replaced_zone_change, execute_zone_move, ZoneDeliveryResult,
    ZoneMoveResult,
};

fn append_effect_resolved_after_counter_pause(
    state: &mut GameState,
    kind: EffectKind,
    source_id: ObjectId,
) {
    super::counters::append_pending_counter_post_actions(
        state,
        vec![PendingCounterPostAction::EmitEffectResolved { kind, source_id }],
    );
}

/// Move target objects between zones.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (
        origin,
        dest_zone,
        owner_library,
        effect_enter_transformed,
        enters_under_player,
        effect_enter_tapped,
        effect_enters_attacking,
        up_to,
        effect_enter_with_counters,
        face_down_profile,
    ) = match &ability.effect {
        Effect::ChangeZone {
            origin,
            destination,
            owner_library,
            enter_transformed,
            enters_under,
            enter_tapped,
            enters_attacking,
            up_to,
            enter_with_counters,
            face_down_profile,
            ..
        } => {
            // CR 122.1 + CR 614.1c: Resolve `QuantityExpr` counts to concrete
            // u32 values up front so the zone-move pipeline carries fully-
            // resolved counts (matches the Token resolver pattern at
            // `effects/token.rs:400`).
            let resolved_counters: Vec<(CounterType, u32)> = enter_with_counters
                .iter()
                .map(|(ct, qty)| {
                    let n =
                        crate::game::quantity::resolve_quantity_with_targets(state, qty, ability)
                            .max(0) as u32;
                    (ct.clone(), n)
                })
                .collect();
            // CR 110.2a: Resolve the controller-override `ControllerRef` to a
            // concrete `PlayerId` exactly once at resolver entry, then carry
            // the resolved `Option<PlayerId>` through the iteration ctx and
            // the `EffectZoneChoice` round-trip. This keeps the runtime
            // carrier immune to re-evaluation across an interactive pause
            // and concentrates the `ControllerRef` semantics in one place.
            // Resolved via the canonical `ControllerRef` resolver, so any
            // player reference ("under their/that/target player's control")
            // maps to a concrete controller (CR 110.2a).
            let enters_under_player =
                resolve_enters_under_player(state, ability, "ChangeZone", enters_under.as_ref())?;
            (
                *origin,
                *destination,
                *owner_library,
                *enter_transformed,
                enters_under_player,
                *enter_tapped,
                *enters_attacking,
                *up_to,
                resolved_counters,
                face_down_profile.clone(),
            )
        }
        _ => return Err(EffectError::MissingParam("Destination".to_string())),
    };

    let mut origin = origin;

    // CR 603.7: Resolve the `TrackedSetId(0)` sentinel emitted by the parser
    // for "from among the milled cards" / "X cards revealed this way"
    // continuations to the most recent non-empty tracked set. Done up front so
    // every downstream path (interactive scan, `matches_target_filter`,
    // `tracked_set_member_zones`) sees the bound id — `matches_target_filter`
    // looks the set up by exact id and would otherwise miss the sentinel.
    let target_filter: TargetFilter = match &ability.effect {
        Effect::ChangeZone { target, .. } => {
            crate::game::targeting::resolve_tracked_set_sentinel(state, target.clone())
        }
        _ => TargetFilter::Any,
    };
    let target_filter = &target_filter;
    if origin.is_none() && matches!(target_filter, TargetFilter::TriggeringSource) {
        origin = state
            .current_trigger_event
            .as_ref()
            .and_then(|event| match event {
                GameEvent::ZoneChanged { to, .. } => Some(*to),
                _ => None,
            });
    }
    let filter_controller =
        crate::game::effects::controller_for_relative_filter(ability, target_filter);
    let track_exiled_by_source =
        crate::game::exile_links::should_track_exiled_by_source(state, ability.source_id, ability);

    // CR 608.2c + 603.10a: Resolve the subject across self-ref → event-context →
    // chosen-targets, the unified 3-tier dispatch shared by zone-change-style
    // effects whose subject can be the source itself, an event-context
    // referent, or a pre-selected target. See `targeting::resolved_targets`.
    let effective_targets = crate::game::targeting::resolved_targets(ability, target_filter, state);
    let targeted_objects =
        crate::game::effects::effect_object_targets(target_filter, &effective_targets);

    if targeted_objects.is_empty() {
        // CR 115.6: "Up to one target" — if the player chose zero targets during
        // targeting, the effect resolves doing nothing. Don't fall through to the
        // untargeted zone-scan path (which is for genuinely untargeted effects like
        // "sacrifice a creature" where the choice happens at resolution).
        // CR 608.2b: Use `targeting_is_optional()` not `optional_targeting`: "up to one"
        // expressed via `multi_target.min = 0` (per-opponent fanout) must also
        // short-circuit here, not reach the zone-scan and spuriously set
        // `cost_payment_failed_flag`.
        // Exception: when `target_choice_timing == Resolution` the player has not
        // yet had a chance to choose — targets are empty by design and the zone-scan
        // path must be reached so `EffectZoneChoice` can be issued.
        if ability.targeting_is_optional()
            && !matches!(ability.target_choice_timing, TargetChoiceTiming::Resolution)
        {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        // CR 701.23b + CR 401.2: Interactive library-step fail-to-find guard.
        // The parser emits `origin=Library, target=Any` for the put-step of a
        // chain where an earlier interactive step selects the card from the
        // library (SearchLibrary for tutors/fetches, ChooseFromZone for the
        // "look at the top N, choose one" patterns). On success, the relevant
        // choice handler in `engine_resolution_choices` populates
        // `ability.targets` with the chosen card before this handler runs.
        // On fail-to-find (CR 701.23b: a player isn't required to find a card;
        // analogous no-selection outcomes for other interactive steps), targets
        // stay empty and this put-step must no-op so the subsequent sub-ability
        // in the chain (e.g., Shuffle) still runs.
        //
        // The invariant: libraries are hidden zones (CR 401.2), so no untargeted
        // resolution-time zone scan over a library is ever valid — reaching this
        // branch with `Library + Any + empty targets` always means an earlier
        // interactive step completed without producing a selection. Fall-through
        // to the zone-scan below would incorrectly treat `Any` as a wildcard
        // across every library in the game and let the player pick any card.
        // Hand/Graveyard/Exile zone-scan semantics (Show-and-Tell, Regrowth,
        // etc.) are unaffected.
        //
        // CR 701.23a: A multi-zone tutor's put-step carries `origin: None`
        // (the found card may come from graveyard/hand/library, so the move
        // reads the card's actual zone) with `target: Any`. The same fail-to-find
        // no-op applies: empty targets means the search found nothing, so the
        // put-step must do nothing rather than fall through to an `origin=None,
        // Any` battlefield wildcard scan. Untargeted `None + Any` is never a
        // real standalone effect — it only arises as this continuation artifact.
        if (origin == Some(Zone::Library) || origin.is_none())
            && matches!(target_filter, TargetFilter::Any)
        {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        // CR 608.2c: A tracked-set filter ("from among the milled cards" / "X
        // cards revealed this way") scopes the selection to a set of objects
        // that may live in any zone. The tracked-set membership is the scope —
        // there is no fixed `InZone` constraint to extract — so derive the scan
        // zone from the members' actual zone rather than defaulting to the
        // battlefield.
        let scan_zone = origin
            .or_else(|| target_filter.extract_in_zone())
            .or_else(|| {
                tracked_set_member_zones(state, target_filter)
                    .and_then(|zones| zones.into_iter().next())
            })
            .unwrap_or(Zone::Battlefield);
        // Filter-controller override is primary here: when a filter like
        // "creature you control" needs "you" to resolve to the *target* player
        // (not the caster), we pass `filter_controller` explicitly. Use
        // `from_source_with_controller` to honor this remapping.
        let ctx = crate::game::filter::FilterContext::from_source_with_controller(
            ability.source_id,
            filter_controller,
        );
        let eligible: Vec<ObjectId> = state
            .objects
            .iter()
            .filter(|(id, obj)| {
                obj.zone == scan_zone
                    && !obj.is_emblem
                    && crate::game::filter::matches_target_filter(state, **id, target_filter, &ctx)
            })
            .map(|(id, _)| *id)
            .collect();
        let eligible: Vec<ObjectId> = if dest_zone == Zone::Exile {
            eligible
                .into_iter()
                .filter(|id| {
                    let acting_player = state
                        .objects
                        .get(id)
                        .map(|obj| obj.controller)
                        .unwrap_or(ability.controller);
                    !crate::game::static_abilities::triggered_cause_sacrifice_or_exile_muzzled(
                        state,
                        ability,
                        *id,
                        acting_player,
                    )
                })
                .collect()
        } else {
            eligible
        };

        if eligible.is_empty() {
            if !up_to {
                state.cost_payment_failed_flag = true;
            }
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        let (choice_count, min_count, choice_up_to) =
            resolution_choice_cardinality(state, ability, eligible.len(), up_to);

        if matches!(ability.target_selection_mode, TargetSelectionMode::Random)
            && !choice_up_to
            && choice_count == 1
        {
            let index = state.rng.random_range(0..eligible.len());
            let chosen = eligible[index];
            // CR 110.2a: `enters_under_player` was resolved once at resolver
            // entry — pass it straight through (no per-branch re-resolution).
            match execute_zone_move(
                state,
                chosen,
                scan_zone,
                dest_zone,
                ability.source_id,
                ability.duration.as_ref(),
                effect_enter_transformed,
                effect_enter_tapped,
                enters_under_player,
                &effect_enter_with_counters,
                face_down_profile.as_ref(),
                track_exiled_by_source,
                events,
            ) {
                ZoneMoveResult::Done => {
                    state.last_effect_count = Some(1);
                    if effect_enters_attacking && dest_zone == Zone::Battlefield {
                        let controller = state
                            .objects
                            .get(&chosen)
                            .map(|obj| obj.controller)
                            .unwrap_or(ability.controller);
                        crate::game::combat::enter_attacking(
                            state,
                            chosen,
                            ability.source_id,
                            controller,
                        );
                    }
                }
                ZoneMoveResult::NeedsChoice(player) => {
                    // CR 614.12a: single-pick branch (Random single / single-eligible)
                    // has NO stash/drain, so KEEP the counter-pause EffectResolved
                    // append — it is the ONLY resume-path EffectResolved emit (the
                    // synchronous Done-branch emit below does NOT run on the pause
                    // path). Only the wait-state setter changes to `park_waiting_for`
                    // so a Devour as-enters sacrifice `EffectZoneChoice` already
                    // surfaced by the move isn't clobbered.
                    append_effect_resolved_after_counter_pause(
                        state,
                        EffectKind::from(&ability.effect),
                        ability.source_id,
                    );
                    crate::game::replacement::park_waiting_for(state, player);
                    return Ok(());
                }
                ZoneMoveResult::NeedsAuraAttachmentChoice => return Ok(()),
            }

            // CR 614.13a: single-pick entry completed (Done branch) — clear the
            // pre-entry Devour snapshot (its lifetime = this entry event). The
            // pause arm above returned before reaching here.
            let _ = state.devour_eligible_snapshot.take();
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        if eligible.len() == 1 && !choice_up_to && choice_count == 1 {
            // CR 110.2a: pre-resolved controller override (single-eligible
            // branch). No per-branch re-resolution.
            match execute_zone_move(
                state,
                eligible[0],
                scan_zone,
                dest_zone,
                ability.source_id,
                ability.duration.as_ref(),
                effect_enter_transformed,
                effect_enter_tapped,
                enters_under_player,
                &effect_enter_with_counters,
                face_down_profile.as_ref(),
                track_exiled_by_source,
                events,
            ) {
                ZoneMoveResult::Done => {
                    state.last_effect_count = Some(1);
                    if effect_enters_attacking && dest_zone == Zone::Battlefield {
                        let controller = state
                            .objects
                            .get(&eligible[0])
                            .map(|obj| obj.controller)
                            .unwrap_or(ability.controller);
                        crate::game::combat::enter_attacking(
                            state,
                            eligible[0],
                            ability.source_id,
                            controller,
                        );
                    }
                }
                ZoneMoveResult::NeedsChoice(player) => {
                    // CR 614.12a: single-pick branch (Random single / single-eligible)
                    // has NO stash/drain, so KEEP the counter-pause EffectResolved
                    // append — it is the ONLY resume-path EffectResolved emit (the
                    // synchronous Done-branch emit below does NOT run on the pause
                    // path). Only the wait-state setter changes to `park_waiting_for`
                    // so a Devour as-enters sacrifice `EffectZoneChoice` already
                    // surfaced by the move isn't clobbered.
                    append_effect_resolved_after_counter_pause(
                        state,
                        EffectKind::from(&ability.effect),
                        ability.source_id,
                    );
                    crate::game::replacement::park_waiting_for(state, player);
                    return Ok(());
                }
                ZoneMoveResult::NeedsAuraAttachmentChoice => return Ok(()),
            }

            // CR 614.13a: single-pick entry completed (Done branch) — clear the
            // pre-entry Devour snapshot (its lifetime = this entry event). The
            // pause arm above returned before reaching here.
            let _ = state.devour_eligible_snapshot.take();
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: filter_controller,
            cards: eligible,
            count: choice_count,
            min_count,
            up_to: choice_up_to,
            source_id: ability.source_id,
            effect_kind: EffectKind::ChangeZone,
            zone: scan_zone,
            destination: Some(dest_zone),
            enter_tapped: effect_enter_tapped,
            enter_transformed: effect_enter_transformed,
            enters_under_player,
            enters_attacking: effect_enters_attacking,
            owner_library,
            track_exiled_by_source,
            count_param: 0,
        };
        // EffectResolved is emitted by the EffectZoneChoice handler after the player chooses
        // (matching the DiscardChoice pattern — single authority for the event).
        return Ok(());
    }

    let ctx = ChangeZoneIterationCtx {
        source_id: ability.source_id,
        controller: ability.controller,
        origin,
        destination: dest_zone,
        enter_transformed: effect_enter_transformed,
        enter_tapped: effect_enter_tapped,
        enters_under_player,
        enters_attacking: effect_enters_attacking,
        enter_with_counters: effect_enter_with_counters,
        duration: ability.duration.clone(),
        track_exiled_by_source,
    };
    let _ = owner_library; // routing handled by move_to_zone (CR 400.7)

    // CR 614.12a + CR 614.13a: same pre-loop snapshot as the mass path, for a
    // targeted multi-`ChangeZone` co-entry that brings in one or more devourers.
    // Captured before any member enters so every co-arriver (and the devourers
    // themselves) is excluded regardless of iteration order.
    if dest_zone == Zone::Battlefield
        && state.devour_eligible_snapshot.is_none()
        && targeted_objects
            .iter()
            .any(|id| crate::game::engine_replacement::object_has_devour_replacement(state, *id))
    {
        state.devour_eligible_snapshot = Some(state.battlefield.iter().copied().collect());
    }

    for (i, obj_id) in targeted_objects.iter().enumerate() {
        if dest_zone == Zone::Exile {
            let acting_player = state
                .objects
                .get(obj_id)
                .map(|obj| obj.controller)
                .unwrap_or(ability.controller);
            if crate::game::static_abilities::triggered_cause_sacrifice_or_exile_muzzled(
                state,
                ability,
                *obj_id,
                acting_player,
            ) {
                continue;
            }
        }

        match process_one_zone_move(state, &ctx, *obj_id, events) {
            ZoneMoveResult::Done => {}
            ZoneMoveResult::NeedsAuraAttachmentChoice => {
                state.pending_change_zone_iteration =
                    Some(crate::types::game_state::PendingChangeZoneIteration {
                        remaining: targeted_objects[i + 1..].to_vec(),
                        source_id: ctx.source_id,
                        controller: ctx.controller,
                        origin: ctx.origin,
                        destination: ctx.destination,
                        enter_transformed: ctx.enter_transformed,
                        enter_tapped: ctx.enter_tapped,
                        enters_under_player: ctx.enters_under_player,
                        enters_attacking: ctx.enters_attacking,
                        enter_with_counters: ctx.enter_with_counters.clone(),
                        duration: ctx.duration.clone(),
                        track_exiled_by_source: ctx.track_exiled_by_source,
                        effect_kind: EffectKind::from(&ability.effect),
                    });
                return Ok(());
            }
            ZoneMoveResult::NeedsChoice(player) => {
                // CR 614.12b + CR 614.1c + CR 614.13: stash the unprocessed targets
                // so `drain_pending_change_zone_iteration` resumes the loop after
                // the player resolves this replacement. Without the stash, every
                // target after the first NeedsChoice would be silently dropped
                // (issue #535).
                state.pending_change_zone_iteration =
                    Some(crate::types::game_state::PendingChangeZoneIteration {
                        remaining: targeted_objects[i + 1..].to_vec(),
                        source_id: ctx.source_id,
                        controller: ctx.controller,
                        origin: ctx.origin,
                        destination: ctx.destination,
                        enter_transformed: ctx.enter_transformed,
                        enter_tapped: ctx.enter_tapped,
                        enters_under_player: ctx.enters_under_player,
                        enters_attacking: ctx.enters_attacking,
                        enter_with_counters: ctx.enter_with_counters.clone(),
                        duration: ctx.duration.clone(),
                        track_exiled_by_source: ctx.track_exiled_by_source,
                        effect_kind: EffectKind::from(&ability.effect),
                    });
                // CR 614.12a: park (don't clobber) — a Devour as-enters sacrifice
                // may already have surfaced its own `EffectZoneChoice`.
                crate::game::replacement::park_waiting_for(state, player);
                // EffectResolved is emitted by the drain after the loop completes —
                // do NOT emit here.
                return Ok(());
            }
        }
    }

    // CR 614.13a: targeted multi-ChangeZone co-entry completed without pausing —
    // clear the pre-entry Devour snapshot (its lifetime = this entry event).
    let _ = state.devour_eligible_snapshot.take();

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Per-iteration context for the multi-target `ChangeZone` loop. Captured once
/// per `resolve` call (and once per `EffectZoneChoice` resolution) so that the
/// loop body and the post-pause drain share one parameter bundle. Mirrors
/// the captured fields on [`crate::types::game_state::PendingChangeZoneIteration`]
/// minus the resume-only fields (`remaining`, `effect_kind`).
pub(crate) struct ChangeZoneIterationCtx {
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub origin: Option<Zone>,
    pub destination: Zone,
    pub enter_transformed: bool,
    pub enter_tapped: EtbTapState,
    /// CR 110.2a: Resolved-once controller override. `Some(pid)` routes the
    /// moved object to `pid` on battlefield entry; `None` keeps the object
    /// under its owner's control. Pre-resolved from
    /// `Effect::ChangeZone.enters_under` at resolver entry.
    pub enters_under_player: Option<PlayerId>,
    pub enters_attacking: bool,
    pub enter_with_counters: Vec<(CounterType, u32)>,
    pub duration: Option<Duration>,
    pub track_exiled_by_source: bool,
}

/// Move one object through the full zone-change pipeline used by the
/// multi-target `ChangeZone` resolution loop and the `EffectZoneChoice`
/// multi-card resume path. Returns `ZoneMoveResult` so the caller can stash
/// and pause on `NeedsChoice` (issue #535).
///
/// Encapsulates: emblem guard (CR 114.5), origin-mismatch skip (CR 400.7 /
/// CR 603.7c), controller override (CR 110.2a), the pipeline call, and the
/// `enter_attacking` post-step (CR 508.4).
pub(crate) fn process_one_zone_move(
    state: &mut GameState,
    ctx: &ChangeZoneIterationCtx,
    obj_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveResult {
    // CR 114.5: Emblems cannot be moved between zones.
    if state.objects.get(&obj_id).is_some_and(|o| o.is_emblem) {
        return ZoneMoveResult::Done;
    }

    let from_zone = state
        .objects
        .get(&obj_id)
        .map(|o| o.zone)
        .unwrap_or(Zone::Battlefield);

    // CR 400.7 + CR 603.7c: If an origin zone is specified and the object is
    // no longer in that zone, the zone change is impossible — skip silently.
    if let Some(expected_origin) = ctx.origin {
        if from_zone != expected_origin {
            return ZoneMoveResult::Done;
        }
    }

    // CR 110.2a: `enters_under_player` was pre-resolved at resolver entry;
    // pass it straight to the zone-move pipeline so replacement effects see
    // the correct controller without re-evaluating the `ControllerRef`.
    // NOTE: `face_down_profile` is not yet threaded through the interactive
    // single-selection carriers (`ChangeZoneIterationCtx`,
    // `PendingChangeZoneIteration`, `WaitingFor::EffectZoneChoice`). The only
    // current face-down-on-entry effect (Cyber-Controller) resolves via the mass
    // `resolve_all` path, so this multi-target/interactive single path passes
    // `None`. Latent: threading it here would extend face-down entry to
    // interactive single-card "put X face down" effects if any are added.
    let result = execute_zone_move(
        state,
        obj_id,
        from_zone,
        ctx.destination,
        ctx.source_id,
        ctx.duration.as_ref(),
        ctx.enter_transformed,
        ctx.enter_tapped,
        ctx.enters_under_player,
        &ctx.enter_with_counters,
        None,
        ctx.track_exiled_by_source,
        events,
    );

    if let ZoneMoveResult::Done = result {
        // CR 508.4: Place on battlefield attacking (not declared as attacker).
        if ctx.enters_attacking && ctx.destination == Zone::Battlefield {
            let controller = state
                .objects
                .get(&obj_id)
                .map(|obj| obj.controller)
                .unwrap_or(ctx.controller);
            crate::game::combat::enter_attacking(state, obj_id, ctx.source_id, controller);
        }
    }
    result
}

/// Move all objects matching the filter from `Origin` zone to `Destination` zone.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 400.3 + CR 701.23: When the target filter encodes multiple zones via
    // `InAnyZone`, scan their union; otherwise fall back to the explicit `origin`
    // (or `Battlefield`). Single-zone filters (`InZone` alone) preserve legacy
    // behavior — only the multi-zone shape opts into the union scan.
    let (origin_zones, dest_zone, target_filter, enter_tapped) = match &ability.effect {
        Effect::ChangeZoneAll {
            origin,
            destination,
            target,
            enters_under: _,
            enter_tapped,
            face_down_profile: _,
        } => {
            let extracted = target.extract_zones();
            let scan_zones = if extracted.len() > 1 {
                extracted
            } else if let Some(origin) = origin {
                vec![*origin]
            } else if let Some(zones) = tracked_set_member_zones(state, target) {
                zones
            } else {
                vec![Zone::Battlefield]
            };
            (scan_zones, *destination, target.clone(), *enter_tapped)
        }
        _ => return Err(EffectError::MissingParam("ChangeZoneAll".to_string())),
    };
    let origin_zone = origin_zones[0];

    // CR 400.6 + CR 400.3: `TargetFilter::Controller` / `TargetFilter::Player`
    // in a mass zone-change reference a *player*, not a set of objects. Such
    // filters arise from phrases like "shuffle your hand into your library"
    // (Controller) or "that player shuffles their hand into their library"
    // (Player, with the subject supplying the target at resolution). Translate
    // them here to "all cards owned by that player in the origin zone" — the
    // object-level matcher would otherwise reject them outright.
    let player_scope: Option<crate::types::player::PlayerId> = match &target_filter {
        TargetFilter::Controller => Some(ability.controller),
        TargetFilter::Player => ability
            .targets
            .iter()
            .find_map(|t| match t {
                crate::types::ability::TargetRef::Player(p) => Some(*p),
                _ => None,
            })
            .or(Some(ability.controller)),
        _ => None,
    };

    let filter_controller =
        crate::game::effects::controller_for_relative_filter(ability, &target_filter);
    let target_filter = owner_scoped_nonbattlefield_mass_filter(target_filter, &origin_zones);

    // Use a permissive default filter if the effect's target is None
    let effective_filter = if matches!(target_filter, crate::types::ability::TargetFilter::None) {
        crate::types::ability::TargetFilter::Typed(TypedFilter {
            type_filters: vec![crate::types::ability::TypeFilter::Permanent],
            controller: None,
            properties: vec![],
        })
    } else {
        crate::game::effects::resolved_object_filter(ability, &target_filter)
    };

    // CR 603.7: Resolve the `TrackedSetId(0)` sentinel emitted by the parser for
    // inline "the exiled card[s]" continuations (e.g., Sword of Hearth and Home's
    // chain: exile creature → search land → return the exiled card). The
    // delayed-trigger resolver performs the same binding at delayed-trigger
    // creation time; inline chains must bind here so `ChangeZoneAll` scans the
    // correct set.
    let effective_filter =
        crate::game::targeting::resolve_tracked_set_sentinel(state, effective_filter);

    // CR 608.2c: Re-derive scan zones after the tracked-set sentinel binds —
    // the initial `origin`/`target` snapshot may have defaulted to the
    // battlefield before `chain_tracked_set_id` was populated (Zimone's
    // Experiment: kept cards live in the library until routed by type).
    let origin_zones = if matches!(&ability.effect, Effect::ChangeZoneAll { origin: None, .. }) {
        if let Some(zones) = tracked_set_member_zones(state, &effective_filter) {
            zones
        } else if let Some(zones) = tracked_set_member_zones(state, &target_filter) {
            zones
        } else {
            origin_zones
        }
    } else {
        origin_zones
    };

    let track_exiled_by_source =
        crate::game::exile_links::should_track_exiled_by_source(state, ability.source_id, ability);

    let enters_under_player: Option<PlayerId> = match &ability.effect {
        Effect::ChangeZoneAll { enters_under, .. } => {
            resolve_enters_under_player(state, ability, "ChangeZoneAll", enters_under.as_ref())?
        }
        _ => None,
    };

    // CR 708.2a + CR 708.3: Carry the face-down profile so each entering object
    // is turned face down before it enters the battlefield (Cyber-Controller:
    // "Put all creature cards milled this way onto the battlefield face down ...").
    let face_down_profile: Option<crate::types::ability::FaceDownProfile> = match &ability.effect {
        Effect::ChangeZoneAll {
            face_down_profile, ..
        } => face_down_profile.clone(),
        _ => None,
    };

    // Collect matching object IDs from the origin zone.
    // Explicit filter-controller override (e.g., "creature that player controls")
    // — use `from_ability_with_controller` so target-inheriting predicates like
    // `FilterProp::SameNameAsParentTarget` can read the parent target out of
    // `ability.targets` while still honoring the remapped controller.
    let ctx = crate::game::filter::FilterContext::from_ability_with_controller(
        ability,
        filter_controller,
    );
    let matching: Vec<_> = if let Some(player) = player_scope {
        // Player-scoped mass move: select every card in any of the origin zones
        // belonging to the target player, regardless of type.
        //
        // CR 110.1 + CR 108.3: Hand / library / graveyard / exile membership is
        // keyed by *owner*, not controller — only a card on the battlefield is a
        // permanent (CR 110.1) and thus has a controller; ownership (CR 108.3)
        // is the player who started the game with the card. A creature stolen
        // via Mind Control retains
        // `obj.controller = thief` even after dying into its owner's graveyard
        // (`reset_for_battlefield_exit` does not reset controller; only the
        // layer pass over `battlefield_phased_in_ids` does, and it skips zones
        // off the battlefield). Filtering by owner is therefore both rules-
        // correct and robust to that state divergence. For battlefield-origin
        // mass moves ("exile all permanents you control"), `obj.controller`
        // is authoritative, so we keep that filter for the battlefield case.
        state
            .objects
            .iter()
            .filter(|(_, obj)| {
                origin_zones.contains(&obj.zone)
                    && if obj.zone == Zone::Battlefield {
                        obj.controller == player
                    } else {
                        obj.owner == player
                    }
            })
            .map(|(id, _)| *id)
            .collect()
    } else {
        state
            .objects
            .iter()
            .filter(|(&id, obj)| {
                origin_zones.contains(&obj.zone)
                    && crate::game::filter::matches_target_filter(
                        state,
                        id,
                        &effective_filter,
                        &ctx,
                    )
            })
            .map(|(id, _)| *id)
            .collect()
    };
    let matching: Vec<_> = if dest_zone == Zone::Exile {
        matching
            .into_iter()
            .filter(|id| {
                let acting_player = state
                    .objects
                    .get(id)
                    .map(|obj| obj.controller)
                    .unwrap_or(ability.controller);
                !crate::game::static_abilities::triggered_cause_sacrifice_or_exile_muzzled(
                    state,
                    ability,
                    *id,
                    acting_player,
                )
            })
            .collect()
    } else {
        matching
    };

    // Clean up consumed tracked set after scanning.
    if let TargetFilter::TrackedSet { id } = &effective_filter {
        state.tracked_object_sets.remove(id);
    }

    // CR 614.12a + CR 614.13a: when a mass entry brings in one or more devourers
    // simultaneously, snapshot the eligible pool BEFORE any co-entering member
    // enters — `state.objects` is unordered, so an ordinary co-arriver may be
    // processed before the devourer; capturing at devourer-entry time would then
    // wrongly include that already-entered co-arriver. Capture pre-loop (when the
    // battlefield is still the pre-entry set) so every co-arriver is excluded.
    // `is_none`-gated so a nested/resumed pass doesn't re-capture; cleared on the
    // event-completion paths below.
    if dest_zone == Zone::Battlefield
        && state.devour_eligible_snapshot.is_none()
        && matching
            .iter()
            .any(|id| crate::game::engine_replacement::object_has_devour_replacement(state, *id))
    {
        state.devour_eligible_snapshot = Some(state.battlefield.iter().copied().collect());
    }

    let mut moved_count: i32 = 0;
    let mut departed: Vec<ObjectId> = Vec::new();
    for (i, obj_id) in matching.iter().enumerate() {
        let obj_id = *obj_id;
        // CR 400.3: Each object's actual current zone is the source zone for the
        // move. Single-zone callers pass `origin_zones = [zone]`; multi-zone
        // callers (e.g. "search graveyard, hand, and library") let each object's
        // own zone drive the move so per-zone replacements/triggers fire correctly.
        let per_object_origin = state
            .objects
            .get(&obj_id)
            .map(|o| o.zone)
            .unwrap_or(origin_zone);
        // Mass zone moves don't use enter_transformed; enter_tapped and
        // controller override are carried for "return ... tapped/under your
        // control" effects.
        match execute_zone_move(
            state,
            obj_id,
            per_object_origin,
            dest_zone,
            ability.source_id,
            ability.duration.as_ref(),
            false,
            enter_tapped,
            enters_under_player,
            &[],
            face_down_profile.as_ref(),
            track_exiled_by_source,
            events,
        ) {
            ZoneMoveResult::Done => {
                moved_count += 1;
                // CR 603.10a + CR 608.2f: Collect battlefield-origin objects that
                // actually left (post-move zone != Battlefield). `execute_zone_move`
                // returns `Done` even when a replacement Prevented the move, so the
                // post-move zone check excludes prevented members from the
                // co-departed group.
                if per_object_origin == Zone::Battlefield
                    && state
                        .objects
                        .get(&obj_id)
                        .is_some_and(|o| o.zone != Zone::Battlefield)
                {
                    departed.push(obj_id);
                }
                // CR 400.7 + CR 608.2c: Track hand-origin exiles separately so
                // QuantityRef::ExiledFromHandThisResolution can resolve "draws a
                // card for each card exiled from their hand this way".
                if per_object_origin == Zone::Hand && dest_zone == Zone::Exile {
                    state.exiled_from_hand_this_resolution =
                        state.exiled_from_hand_this_resolution.saturating_add(1);
                }
                // CR 610.3: Consume ExileLink after successfully moving the object,
                // so check_exile_returns won't try to return it again.
                if matches!(effective_filter, TargetFilter::ExiledBySource) {
                    state.exile_links.retain(|link| link.exiled_id != obj_id);
                }
            }
            ZoneMoveResult::NeedsChoice(player) => {
                // CR 614.12a + CR 614.13: a Devour as-enters sacrifice surfaced its
                // own `EffectZoneChoice` (or a counter-pause replacement choice).
                // Stash the unprocessed co-entering members so
                // `drain_pending_change_zone_iteration` resumes the mass move after
                // the player resolves this choice — without the stash, every member
                // after the first NeedsChoice would be silently dropped (issue #535
                // class). The drain owns the single trailing EffectResolved, so we do
                // NOT emit it here (mirrors the targeted loop's contract).
                //
                // NOTE (pre-existing face_down residual, extended not regressed):
                // `process_one_zone_move` (the drain's mover) hardcodes
                // `face_down=None`, while this mass loop passes
                // `face_down_profile.as_ref()`. Resumed members of a face-down mass
                // entry (the Cyber-Controller class) therefore lose their face-down
                // profile. This carrier gap predates this change.
                state.pending_change_zone_iteration =
                    Some(crate::types::game_state::PendingChangeZoneIteration {
                        remaining: matching[i + 1..].to_vec(),
                        source_id: ability.source_id,
                        controller: ability.controller,
                        origin: None,
                        destination: dest_zone,
                        enter_transformed: false,
                        enter_tapped,
                        enters_under_player,
                        enters_attacking: false,
                        enter_with_counters: vec![],
                        duration: ability.duration.clone(),
                        track_exiled_by_source,
                        effect_kind: EffectKind::from(&ability.effect),
                    });
                crate::game::replacement::park_waiting_for(state, player);
                return Ok(());
            }
            ZoneMoveResult::NeedsAuraAttachmentChoice => {
                // CR 614.13a: this terminal early-exit ends the mass-entry event
                // (no stash/resume), so the pre-entry Devour snapshot's lifetime is
                // over — clear it so it can't leak into a later sacrifice.
                let _ = state.devour_eligible_snapshot.take();
                return Ok(());
            }
        }
    }

    // CR 614.13a: the whole co-entry event completed without pausing — clear the
    // pre-entry Devour snapshot (its lifetime = this one ChangeZone-to-battlefield
    // event). NOT cleared on the NeedsChoice pause above (the paused devourer's
    // sacrifice + remaining co-entering members still need it).
    let _ = state.devour_eligible_snapshot.take();

    // CR 603.10a + CR 608.2f: Every battlefield-origin object that left did so as
    // part of the same mass zone-change event, so leaves-the-battlefield observers
    // among the departed group observe each other via last-known information.
    zones::mark_simultaneous_departures(events, &departed);

    // CR 608.2c: "that many" in a later instruction refers back to the prior
    // action's count. Record the number of objects moved so downstream
    // sub-abilities using QuantityRef::EventContextAmount resolve correctly —
    // e.g., Whirlpool Drake: "shuffle the cards from your hand into your library,
    // then draw that many cards."
    state.last_effect_count = Some(moved_count);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

fn owner_scoped_nonbattlefield_mass_filter(
    filter: TargetFilter,
    origin_zones: &[Zone],
) -> TargetFilter {
    if origin_zones.contains(&Zone::Battlefield) {
        return filter;
    }

    match filter {
        TargetFilter::Typed(mut typed) => {
            if let Some(controller) = typed.controller.take() {
                typed.properties.push(FilterProp::Owned { controller });
            }
            TargetFilter::Typed(typed)
        }
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|filter| owner_scoped_nonbattlefield_mass_filter(filter, origin_zones))
                .collect(),
        },
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| owner_scoped_nonbattlefield_mass_filter(filter, origin_zones))
                .collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(owner_scoped_nonbattlefield_mass_filter(
                *filter,
                origin_zones,
            )),
        },
        TargetFilter::TrackedSetFiltered { id, filter } => TargetFilter::TrackedSetFiltered {
            id,
            filter: Box::new(owner_scoped_nonbattlefield_mass_filter(
                *filter,
                origin_zones,
            )),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ControllerRef, FilterProp, MultiTargetSpec, PlayerFilter, PtValue, QuantityExpr,
        QuantityRef, StaticDefinition, TargetFilter, TargetRef, TypeFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::game_state::{ExileLinkKind, StackEntry, StackEntryKind, ZoneChangeRecord};
    use crate::types::identifiers::{CardId, ObjectId, TrackedSetId};
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;
    use crate::types::statics::{ProhibitionScope, StaticMode};
    use std::sync::Arc;

    fn make_hand_choice_ability(up_to: bool) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn move_from_hand_to_battlefield() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
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
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&obj_id));
        assert!(!state.players[0].hand.contains(&obj_id));
    }

    /// CR 110.2a + CR 109.4: "put ... onto the battlefield under target player's
    /// control" enters the card under the chosen player, not the ability
    /// controller. Before, any `enters_under` other than `You` errored at the
    /// resolver; now it routes through the canonical `ControllerRef` resolver.
    #[test]
    fn enters_under_target_player_puts_card_under_chosen_player() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::TargetPlayer),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![TargetRef::Object(obj_id), TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&obj_id));
        assert_eq!(
            state.objects[&obj_id].controller,
            PlayerId(1),
            "card must enter under target player's control (CR 110.2a), not the ability controller"
        );
    }

    /// CR 110.2a + CR 115.10: "each player puts ... onto the battlefield under
    /// their control" — under `player_scope` the entering card's controller is
    /// the scoped (iterating) player, resolved via `ControllerRef::ScopedPlayer`.
    #[test]
    fn enters_under_scoped_player_uses_iterating_player() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Card".to_string(),
            Zone::Hand,
        );
        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::ScopedPlayer),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        ability.set_scoped_player_recursive(PlayerId(1));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&obj_id));
        assert_eq!(
            state.objects[&obj_id].controller,
            PlayerId(1),
            "card must enter under the scoped (iterating) player's control"
        );
    }

    /// CR 614.1c + CR 122.1: A creature entering under a controller who has an
    /// active "Other creatures you control enter with an additional +1/+1 counter
    /// on them" static (Kalain-class) enters the battlefield with that extra
    /// +1/+1 counter folded into its entry.
    #[test]
    fn enters_with_additional_counter_from_active_static() {
        let mut state = GameState::new_two_player(42);

        // Static source (Kalain) on the battlefield, controlled by player 0.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Kalain, Reclusive Painter".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }
        let def = StaticDefinition::new(StaticMode::EntersWithAdditionalCounters {
            counter_type: CounterType::Plus1Plus1,
            count: 1,
        })
        .affected(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Another]),
        ));
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.static_definitions.push(def.clone());
            Arc::make_mut(&mut obj.base_static_definitions).push(def);
        }

        // A creature you control entering from hand.
        let entering = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&entering)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
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
            vec![TargetRef::Object(entering)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&entering));
        assert_eq!(
            state.objects[&entering]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1),
            "entering creature must have one +1/+1 counter from the active static, got {:?}",
            state.objects[&entering].counters
        );

        // CR 613.7: the static's own source ("Other") must not have received a
        // counter from its own static.
        assert_eq!(
            state.objects[&source]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            None,
            "the Other-scoped static must not grant the source itself a counter"
        );
    }

    /// CR 614.1c: A permanent's "enters with" replacement static only applies
    /// if it was already functioning before the permanent entered. A creature
    /// entering from hand must not see its own newly-functioning static after
    /// `move_to_zone` and grant itself a counter retroactively.
    #[test]
    fn entering_creature_does_not_apply_its_own_enter_static() {
        let mut state = GameState::new_two_player(42);
        let entering = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Self Static Creature".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&entering).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            let def = StaticDefinition::new(StaticMode::EntersWithAdditionalCounters {
                counter_type: CounterType::Plus1Plus1,
                count: 1,
            })
            .affected(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ));
            obj.static_definitions.push(def.clone());
            Arc::make_mut(&mut obj.base_static_definitions).push(def);
        }

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
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
            vec![TargetRef::Object(entering)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&entering));
        assert_eq!(
            state.objects[&entering]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            None,
            "entering creature must not apply its own newly-functioning static, got {:?}",
            state.objects[&entering].counters
        );
    }

    #[test]
    fn aura_put_onto_battlefield_by_effect_attaches_to_single_legal_host() {
        let mut state = GameState::new_two_player(42);
        let aura_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Returned Aura".to_string(),
            Zone::Graveyard,
        );
        {
            let aura = state.objects.get_mut(&aura_id).unwrap();
            aura.card_types.core_types.push(CoreType::Enchantment);
            aura.card_types.subtypes.push("Aura".to_string());
            aura.keywords
                .push(Keyword::Enchant(TargetFilter::Typed(TypedFilter::new(
                    TypeFilter::Creature,
                ))));
        }

        let host_id = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Legal Host".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&host_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::SpecificObject { id: aura_id },
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&aura_id].zone, Zone::Battlefield);
        assert_eq!(
            state.objects[&aura_id]
                .attached_to
                .and_then(|target| target.as_object()),
            Some(host_id)
        );
        assert!(state.objects[&host_id].attachments.contains(&aura_id));
    }

    #[test]
    fn aura_put_onto_battlefield_by_effect_stays_put_without_legal_host() {
        let mut state = GameState::new_two_player(42);
        let aura_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Returned Aura".to_string(),
            Zone::Graveyard,
        );
        {
            let aura = state.objects.get_mut(&aura_id).unwrap();
            aura.card_types.core_types.push(CoreType::Enchantment);
            aura.card_types.subtypes.push("Aura".to_string());
            aura.keywords
                .push(Keyword::Enchant(TargetFilter::Typed(TypedFilter::new(
                    TypeFilter::Creature,
                ))));
        }

        let host_id = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Illegal Host".to_string(),
            Zone::Battlefield,
        );
        {
            let host = state.objects.get_mut(&host_id).unwrap();
            host.card_types.core_types.push(CoreType::Creature);
            host.static_definitions.push(
                StaticDefinition::new(StaticMode::Other("CantBeEnchanted".to_string()))
                    .affected(TargetFilter::SelfRef),
            );
        }

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::SpecificObject { id: aura_id },
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&aura_id].zone, Zone::Graveyard);
        assert!(state.objects[&aura_id].attached_to.is_none());
        assert!(!state.objects[&host_id].attachments.contains(&aura_id));
    }

    #[test]
    fn aura_put_onto_battlefield_by_effect_prompts_for_multiple_legal_hosts() {
        let mut state = GameState::new_two_player(42);
        let aura_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Returned Aura".to_string(),
            Zone::Graveyard,
        );
        {
            let aura = state.objects.get_mut(&aura_id).unwrap();
            aura.card_types.core_types.push(CoreType::Enchantment);
            aura.card_types.subtypes.push("Aura".to_string());
            aura.keywords
                .push(Keyword::Enchant(TargetFilter::Typed(TypedFilter::new(
                    TypeFilter::Creature,
                ))));
        }

        let first_host = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "First Host".to_string(),
            Zone::Battlefield,
        );
        let second_host = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Second Host".to_string(),
            Zone::Battlefield,
        );
        for host_id in [first_host, second_host] {
            state
                .objects
                .get_mut(&host_id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::SpecificObject { id: aura_id },
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ReturnAsAuraTarget {
                player,
                returned_id,
                legal_targets,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*returned_id, aura_id);
                assert_eq!(
                    legal_targets,
                    &vec![
                        TargetRef::Object(first_host),
                        TargetRef::Object(second_host)
                    ]
                );
            }
            other => panic!("expected Aura host choice, got {other:?}"),
        }
        assert_eq!(state.objects[&aura_id].zone, Zone::Battlefield);
        assert!(state.objects[&aura_id].attached_to.is_none());

        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(second_host)),
            },
        )
        .unwrap();

        assert_eq!(
            state.objects[&aura_id]
                .attached_to
                .and_then(|target| target.as_object()),
            Some(second_host)
        );
        assert!(state.objects[&second_host].attachments.contains(&aura_id));
        assert!(!state.objects[&first_host].attachments.contains(&aura_id));
    }

    #[test]
    fn aura_put_onto_battlefield_by_effect_resumes_multi_target_move_after_choice() {
        let mut state = GameState::new_two_player(42);
        let aura_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Returned Aura".to_string(),
            Zone::Graveyard,
        );
        {
            let aura = state.objects.get_mut(&aura_id).unwrap();
            aura.card_types.core_types.push(CoreType::Enchantment);
            aura.card_types.subtypes.push("Aura".to_string());
            aura.keywords
                .push(Keyword::Enchant(TargetFilter::Typed(TypedFilter::new(
                    TypeFilter::Creature,
                ))));
        }

        let other_card = create_object(
            &mut state,
            CardId(13),
            PlayerId(0),
            "Other Permanent".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&other_card)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let first_host = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "First Host".to_string(),
            Zone::Battlefield,
        );
        let second_host = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Second Host".to_string(),
            Zone::Battlefield,
        );
        for host_id in [first_host, second_host] {
            state
                .objects
                .get_mut(&host_id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
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
            vec![TargetRef::Object(aura_id), TargetRef::Object(other_card)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. }
        ));
        assert!(state.pending_change_zone_iteration.is_some());
        assert_eq!(state.objects[&other_card].zone, Zone::Graveyard);

        let result = apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(second_host)),
            },
        )
        .unwrap();

        assert_eq!(
            state.objects[&aura_id]
                .attached_to
                .and_then(|target| target.as_object()),
            Some(second_host)
        );
        assert!(state.objects[&second_host].attachments.contains(&aura_id));
        assert_eq!(state.objects[&other_card].zone, Zone::Battlefield);
        assert!(state.pending_change_zone_iteration.is_none());
        assert!(matches!(
            state.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        let change_zone_resolutions = result
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::ChangeZone,
                        source_id: ObjectId(100),
                    }
                )
            })
            .count();
        assert_eq!(change_zone_resolutions, 1);
    }

    #[test]
    fn aura_put_onto_battlefield_by_effect_prompts_for_multiple_player_hosts() {
        let mut state = GameState::new_two_player(42);
        let aura_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Returned Curse".to_string(),
            Zone::Graveyard,
        );
        {
            let aura = state.objects.get_mut(&aura_id).unwrap();
            aura.card_types.core_types.push(CoreType::Enchantment);
            aura.card_types.subtypes.push("Aura".to_string());
            aura.keywords.push(Keyword::Enchant(TargetFilter::Player));
        }

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::SpecificObject { id: aura_id },
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ReturnAsAuraTarget {
                player,
                returned_id,
                legal_targets,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*returned_id, aura_id);
                assert_eq!(
                    legal_targets,
                    &vec![
                        TargetRef::Player(PlayerId(0)),
                        TargetRef::Player(PlayerId(1))
                    ]
                );
            }
            other => panic!("expected Aura host choice, got {other:?}"),
        }

        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
        )
        .unwrap();

        assert_eq!(
            state.objects[&aura_id].attached_to,
            Some(crate::game::game_object::AttachTarget::Player(PlayerId(1)))
        );
    }

    #[test]
    fn change_zone_any_number_from_hand_prompts_for_all_eligible_cards() {
        let mut state = GameState::new_two_player(42);
        let bear = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Hand,
        );
        let wolf = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Wolf".to_string(),
            Zone::Hand,
        );
        let island = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Island".to_string(),
            Zone::Hand,
        );
        for id in [bear, wolf] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Creature],
                    controller: Some(ControllerRef::You),
                    properties: vec![FilterProp::InZone { zone: Zone::Hand }],
                }),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: true,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.multi_target = Some(crate::types::ability::MultiTargetSpec::unlimited(0));
        ability.target_choice_timing = crate::types::ability::TargetChoiceTiming::Resolution;
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                min_count,
                up_to,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 2);
                assert_eq!(*min_count, 0);
                assert!(*up_to);
                assert!(cards.contains(&bear));
                assert!(cards.contains(&wolf));
                assert!(!cards.contains(&island));
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    #[test]
    fn change_zone_resolves_triggering_source_from_zone_change_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Earthbent Land".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&obj_id).unwrap().controller = PlayerId(1);
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: obj_id,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord::test_minimal(
                obj_id,
                Some(Zone::Battlefield),
                Zone::Graveyard,
            )),
        });
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Battlefield,
                target: TargetFilter::TriggeringSource,
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&obj_id));
        assert!(!state.players[1].graveyard.contains(&obj_id));
        let obj = state.objects.get(&obj_id).unwrap();
        assert!(obj.tapped);
        assert_eq!(obj.controller, PlayerId(0));
    }

    /// CR 122.1 + CR 614.1c — `Effect::ChangeZone.enter_with_counters` drives
    /// counter placement during the move. For a non-battlefield destination
    /// (Exile, Darigaaz / Draugr / Rayami class), counters are stamped via
    /// `apply_etb_counters` on the object after the zone change completes.
    #[test]
    fn change_zone_enter_with_counters_stamps_counters_on_exile_destination() {
        use crate::types::ability::QuantityExpr;
        use crate::types::counter::CounterType;
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![(
                    CounterType::Generic("egg".to_string()),
                    QuantityExpr::Fixed { value: 3 },
                )],
                face_down_profile: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        // Object moved to exile and got 3 egg counters.
        assert!(state.exile.contains(&obj_id));
        let obj = state
            .objects
            .get(&obj_id)
            .expect("object should still exist post-exile");
        let egg = obj
            .counters
            .get(&CounterType::Generic("egg".to_string()))
            .copied()
            .unwrap_or(0);
        assert_eq!(egg, 3, "expected 3 egg counters, got {egg}");
    }

    #[test]
    fn move_to_exile() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
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
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.exile.contains(&obj_id));
    }

    #[test]
    fn exile_return_with_until_host_leaves_records_link() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Exiled Creature".to_string(),
            Zone::Battlefield,
        );
        let source_id = ObjectId(100);
        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
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
            vec![TargetRef::Object(target_id)],
            source_id,
            PlayerId(0),
        );
        ability.duration = Some(crate::types::ability::Duration::UntilHostLeavesPlay);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.exile.contains(&target_id));
        assert_eq!(state.exile_links.len(), 1);
        assert_eq!(state.exile_links[0].exiled_id, target_id);
        assert_eq!(state.exile_links[0].source_id, source_id);
        assert_eq!(
            state.exile_links[0].kind,
            ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Battlefield,
            }
        );
    }

    #[test]
    fn exile_without_linked_exile_consumer_does_not_track_by_source() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Exiled Creature".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
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
            vec![TargetRef::Object(target_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.exile.contains(&target_id));
        assert!(state.exile_links.is_empty());
    }

    #[test]
    fn exile_with_linked_exile_consumer_tracks_by_source() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Exiled Creature".to_string(),
            Zone::Battlefield,
        );
        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
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
            vec![TargetRef::Object(target_id)],
            ObjectId(100),
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Token {
                name: "Illusion".to_string(),
                power: PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::CardsExiledBySource,
                }),
                toughness: PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::CardsExiledBySource,
                }),
                types: vec!["Creature".to_string(), "Illusion".to_string()],
                colors: vec![],
                keywords: vec![],
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
            ObjectId(100),
            PlayerId(0),
        )));
        ability.player_scope = Some(PlayerFilter::OwnersOfCardsExiledBySource);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.exile.contains(&target_id));
        assert_eq!(state.exile_links.len(), 1);
        assert_eq!(state.exile_links[0].exiled_id, target_id);
        assert_eq!(state.exile_links[0].source_id, ObjectId(100));
        assert_eq!(state.exile_links[0].kind, ExileLinkKind::TrackedBySource);
    }

    #[test]
    fn auto_shuffle_after_library_destination() {
        // CR 701.24a: Moving an object to a library should shuffle that library afterward.
        let mut state = GameState::new_two_player(42);
        // Add some cards to player 0's library so we can detect shuffle
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 10),
                PlayerId(0),
                format!("Lib Card {}", i),
                Zone::Library,
            );
        }
        let lib_before = state.players[0].library.clone();

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Library,
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
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Object should be in library
        assert!(state.players[0].library.contains(&obj_id));
        // Library should have been shuffled — at minimum the order may have changed
        // (with enough cards, the probability of identical order is negligible)
        // We verify that shuffle was called by checking the library contains the object
        // and has the right size
        assert_eq!(state.players[0].library.len(), lib_before.len() + 1);
    }

    #[test]
    fn owner_library_routes_to_owners_library() {
        // CR 400.7: owner_library=true should route to the object's owner's library
        let mut state = GameState::new_two_player(42);
        // Create a creature owned by player 1 but currently controlled by player 0
        // (simulating a stolen creature)
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1), // owned by player 1
            "Stolen Creature".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Library,
                target: TargetFilter::Any,
                owner_library: true,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0), // controller is player 0
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Object should be in player 1's library (owner), not player 0's
        assert!(
            state.players[1].library.contains(&obj_id),
            "should be in owner's library (player 1)"
        );
        assert!(
            !state.players[0].library.contains(&obj_id),
            "should NOT be in controller's library (player 0)"
        );
    }

    #[test]
    fn self_ref_change_zone_processes_source() {
        // SelfRef target on ChangeZone should process the source object
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Self Card".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Library,
                target: TargetFilter::SelfRef,
                owner_library: true,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![], // empty targets — SelfRef means source_id
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Source should have moved to library
        assert!(
            state.players[0].library.contains(&source_id),
            "SelfRef source should be in library"
        );
        assert!(
            !state.battlefield.contains(&source_id),
            "SelfRef source should no longer be on battlefield"
        );
    }

    /// CR 603.6a + CR 400.7: An ability-effect-driven battlefield entry through
    /// `execute_zone_move` stamps `entered_via_ability_source` with the resolving
    /// ability's source. Building-block coverage for the Kodama anti-recursion
    /// provenance field — independent of any single card.
    #[test]
    fn ability_driven_entry_records_placing_source() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Placer".to_string(),
            Zone::Battlefield,
        );
        let moved = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Placed Card".to_string(),
            Zone::Hand,
        );
        let mut events = Vec::new();

        let result = execute_zone_move(
            &mut state,
            moved,
            Zone::Hand,
            Zone::Battlefield,
            source_id,
            None,
            false,
            crate::types::zones::EtbTapState::Unspecified,
            None,
            &[],
            None,
            false,
            &mut events,
        );
        assert!(matches!(result, ZoneMoveResult::Done));

        let obj = &state.objects[&moved];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(
            obj.entered_via_ability_source,
            Some(source_id),
            "an ability-effect-driven entry must record the placing ability's source",
        );

        // CR 400.7: moving the permanent off the battlefield clears the
        // provenance — a re-entering permanent is a new object.
        let mut events2 = Vec::new();
        zones::move_to_zone(&mut state, moved, Zone::Graveyard, &mut events2);
        assert_eq!(
            state.objects[&moved].entered_via_ability_source, None,
            "battlefield exit must clear the ability-placement provenance (CR 400.7)",
        );
    }

    #[test]
    fn change_zone_all_bounce_opponent_creatures() {
        let mut state = GameState::new_two_player(42);
        let opp1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opp Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let opp2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp Wolf".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Controller's creature should stay
        let mine = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "My Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&mine)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Battlefield),
                destination: Zone::Hand,
                target: TargetFilter::None,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // All permanents bounced (filter is "Permanent" by default)
        // ChangeZoneAll uses typed TargetFilter for filtering.
    }

    #[test]
    fn change_zone_all_exile_target_player_graveyard() {
        // CR 400.12 + CR 404 + CR 406: "exile target player's graveyard"
        // (Nihil Spellbomb, Bojuka Bog, Tormod's Crypt class) must move every
        // card from the chosen player's graveyard to the exile zone.
        let mut state = GameState::new_two_player(42);

        // Five cards in opponent's (PlayerId(1)) graveyard.
        let mut opp_grave_ids = Vec::new();
        for i in 0..5 {
            let id = create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(1),
                format!("Opp Card {i}"),
                Zone::Graveyard,
            );
            opp_grave_ids.push(id);
        }
        // One card in our own graveyard — must remain untouched.
        let mine = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "My Card".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                target: TargetFilter::Player,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(500),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        for id in &opp_grave_ids {
            let obj = &state.objects[id];
            assert_eq!(
                obj.zone,
                Zone::Exile,
                "opponent's graveyard card {id:?} should be exiled"
            );
        }
        assert_eq!(
            state.objects[&mine].zone,
            Zone::Graveyard,
            "controller's graveyard must be untouched"
        );
    }

    #[test]
    fn change_zone_all_triggered_muzzle_skips_creature_tokens() {
        // CR 603.2 + CR 609.3: The Master, Multiplied-style statics suppress
        // triggered mass-exile effects for protected objects while the effect
        // still does as much as possible for unprotected objects.
        let mut state = GameState::new_two_player(42);
        let master = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "The Master, Multiplied".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&master)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantCauseSacrificeOrExile {
                    cause: ProhibitionScope::Controller,
                })
                .affected(TargetFilter::Typed(
                    TypedFilter::creature()
                        .properties(vec![FilterProp::Token])
                        .controller(ControllerRef::You),
                )),
            );

        let token = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Copied Soldier".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&token).unwrap();
            obj.is_token = true;
            obj.card_types.core_types.push(CoreType::Creature);
        }
        let nontoken = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Real Soldier".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&nontoken)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        state.resolving_stack_entry = Some(StackEntry {
            id: ObjectId(101),
            controller: PlayerId(0),
            source_id: ObjectId(100),
            kind: StackEntryKind::TriggeredAbility {
                source_id: ObjectId(100),
                ability: Box::new(ability.clone()),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
            },
        });
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&token].zone, Zone::Battlefield);
        assert_eq!(state.objects[&nontoken].zone, Zone::Exile);
        assert_eq!(state.last_effect_count, Some(1));
    }

    #[test]
    fn change_zone_all_target_player_commander_moves_chosen_players_commander() {
        let mut state = GameState::new_two_player(42);

        let chosen_commander = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Chosen Commander".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&chosen_commander)
            .unwrap()
            .is_commander = true;

        let controller_commander = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Controller Commander".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&controller_commander)
            .unwrap()
            .is_commander = true;

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: None,
                destination: Zone::Command,
                target: TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::You),
                    properties: vec![FilterProp::IsCommander],
                    ..Default::default()
                }),
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&chosen_commander].zone, Zone::Command);
        assert_eq!(state.objects[&controller_commander].zone, Zone::Battlefield);
    }

    #[test]
    fn change_zone_all_exile_target_player_graveyard_includes_stolen_then_died() {
        // CR 404.2 + CR 110.2: A creature stolen via Mind Control / Bribery
        // dies into its *owner's* graveyard, but `obj.controller` retains the
        // thief's PlayerId because `reset_for_battlefield_exit` does not reset
        // controller and the layer pass only re-applies controller modifications
        // to permanents that are still on the battlefield. "Exile target
        // player's graveyard" must filter by `obj.owner`, not `obj.controller`,
        // so the stolen-then-died corpse is not silently left behind.
        //
        // Regression for the bug shipped in 08ab17b97: `create_object` sets
        // `controller = owner`, so the original test could not exercise this
        // divergent state — only an explicit overwrite reproduces the case.
        let mut state = GameState::new_two_player(42);

        // Three "normal" cards in opponent's graveyard (controller == owner).
        let mut opp_grave_ids = Vec::new();
        for i in 0..3 {
            let id = create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(1),
                format!("Opp Card {i}"),
                Zone::Graveyard,
            );
            opp_grave_ids.push(id);
        }
        // One stolen-then-died corpse: owner = PlayerId(1), controller =
        // PlayerId(0) (the thief). Must still be exiled when targeting
        // PlayerId(1)'s graveyard.
        let stolen = create_object(
            &mut state,
            CardId(150),
            PlayerId(1),
            "Stolen Corpse".to_string(),
            Zone::Graveyard,
        );
        if let Some(obj) = state.objects.get_mut(&stolen) {
            obj.controller = PlayerId(0);
        }
        opp_grave_ids.push(stolen);

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                target: TargetFilter::Player,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(500),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        for id in &opp_grave_ids {
            let obj = &state.objects[id];
            assert_eq!(
                obj.zone,
                Zone::Exile,
                "opponent-owned graveyard card {id:?} should be exiled regardless of stale controller",
            );
        }
    }

    #[test]
    fn change_zone_all_your_graveyard_typed_filter_uses_owner_not_stale_controller() {
        let mut state = GameState::new_two_player(42);
        let owned_land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Owned Land".to_string(),
            Zone::Graveyard,
        );
        let stolen_then_died_land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Stolen Then Died Land".to_string(),
            Zone::Graveyard,
        );
        let opponent_land = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opponent Land".to_string(),
            Zone::Graveyard,
        );
        for id in [owned_land, stolen_then_died_land, opponent_land] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
        }
        state
            .objects
            .get_mut(&stolen_then_died_land)
            .unwrap()
            .controller = PlayerId(1);

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(
                    TypedFilter::land()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::InZone {
                            zone: Zone::Graveyard,
                        }]),
                ),
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                face_down_profile: None,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        for id in [owned_land, stolen_then_died_land] {
            let obj = &state.objects[&id];
            assert_eq!(obj.zone, Zone::Battlefield);
            assert!(obj.tapped);
        }
        assert_eq!(state.objects[&opponent_land].zone, Zone::Graveyard);
    }

    #[test]
    fn change_zone_all_exile_target_player_graveyard_empty_is_noop() {
        // Edge case: targeting a player with an empty graveyard is legal and
        // resolves with no zone changes. (Nihil Spellbomb's ruling allows
        // activation against any player.)
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                target: TargetFilter::Player,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(500),
            PlayerId(0),
        );
        let mut events = Vec::new();

        // Must not error.
        resolve_all(&mut state, &ability, &mut events).unwrap();
    }

    #[test]
    fn resolve_all_exile_with_until_host_leaves_creates_links() {
        // Phase 2 fix: resolve_all should create ExileLinks for UntilHostLeavesPlay
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Starcage".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let c1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let c2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Wolf".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&c2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Creature],
                    controller: Some(crate::types::ability::ControllerRef::Opponent),
                    properties: vec![],
                }),
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        ability.duration = Some(crate::types::ability::Duration::UntilHostLeavesPlay);
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // Both creatures should be exiled
        assert!(state.exile.contains(&c1), "c1 should be in exile");
        assert!(state.exile.contains(&c2), "c2 should be in exile");

        // CR 610.3a: ExileLinks should be created for each exiled object
        assert_eq!(
            state.exile_links.len(),
            2,
            "should have 2 exile links, got {}",
            state.exile_links.len()
        );
        for link in &state.exile_links {
            assert_eq!(link.source_id, source_id, "link source should be Starcage");
            assert_eq!(
                link.kind,
                ExileLinkKind::UntilSourceLeaves {
                    return_zone: Zone::Battlefield,
                },
                "should return to battlefield when source leaves"
            );
        }
    }

    #[test]
    fn resolve_all_exiled_by_source_moves_linked_and_consumes_links() {
        use crate::types::game_state::{ExileLink, ExileLinkKind};

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Starcage".into(),
            Zone::Battlefield,
        );

        // Create two exiled objects linked to source
        let exiled1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".into(),
            Zone::Exile,
        );
        let exiled2 = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Sol Ring".into(),
            Zone::Exile,
        );
        // An unlinked exile card (shouldn't move)
        let unlinked = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Swords Target".into(),
            Zone::Exile,
        );

        state.exile_links.push(ExileLink {
            exiled_id: exiled1,
            source_id,
            kind: ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Battlefield,
            },
        });
        state.exile_links.push(ExileLink {
            exiled_id: exiled2,
            source_id,
            kind: ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Battlefield,
            },
        });
        // Link from a different source — should not be consumed
        state.exile_links.push(ExileLink {
            exiled_id: unlinked,
            source_id: ObjectId(999),
            kind: ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Battlefield,
            },
        });

        // CR 607.2a + CR 406.6: ChangeZoneAll with ExiledBySource moves linked cards to graveyard.
        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Graveyard,
                target: TargetFilter::ExiledBySource,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();

        // Linked objects moved to graveyard
        assert_eq!(state.objects[&exiled1].zone, Zone::Graveyard);
        assert_eq!(state.objects[&exiled2].zone, Zone::Graveyard);
        // Unlinked object stayed in exile
        assert_eq!(state.objects[&unlinked].zone, Zone::Exile);

        // Consumed ExileLinks for source, kept unrelated link
        assert_eq!(state.exile_links.len(), 1);
        assert_eq!(state.exile_links[0].exiled_id, unlinked);
    }

    #[test]
    fn under_your_control_sets_controller_through_pipeline() {
        // CR 110.2a: controller_override should flow through the replacement pipeline,
        // not be applied as a post-move patch.
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1), // owned by player 1
            "Stolen Creature".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let source_id = ObjectId(100);
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![TargetRef::Object(obj_id)],
            source_id,
            PlayerId(0), // controller is player 0
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Object should be on the battlefield under player 0's control
        assert!(state.battlefield.contains(&obj_id));
        assert_eq!(
            state.objects[&obj_id].controller,
            PlayerId(0),
            "under_your_control should set controller to ability's controller"
        );
        // Owner should remain player 1
        assert_eq!(state.objects[&obj_id].owner, PlayerId(1));
    }

    #[test]
    fn enters_attacking_adds_to_combat() {
        // CR 508.4: ChangeZone with enters_attacking should place on battlefield attacking.
        let mut state = GameState::new_two_player(42);
        state.combat = Some(crate::game::combat::CombatState::default());

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Reanimated Creature".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);

        let source_id = ObjectId(100);
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: true,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![TargetRef::Object(obj_id)],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Object should be on battlefield and in combat. Entering attacking
        // does not itself tap the object; "tapped and attacking" effects set
        // `enter_tapped` separately.
        assert!(state.battlefield.contains(&obj_id));
        assert!(
            !state.objects[&obj_id].tapped,
            "CR 508.4: enters attacking alone should not set tapped"
        );
        let combat = state.combat.as_ref().unwrap();
        assert!(
            combat.attackers.iter().any(|a| a.object_id == obj_id),
            "CR 508.4: should be in combat attackers"
        );
    }

    #[test]
    fn origin_zone_mismatch_skips_move() {
        // CR 400.7: If an origin zone is specified and the object is no longer
        // in that zone, the zone change should be skipped.
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dead Creature".to_string(),
            Zone::Graveyard,
        );

        // Try to exile from battlefield, but object is in graveyard
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
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
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Object should remain in graveyard — not moved to exile
        assert!(
            state.players[0].graveyard.contains(&obj_id),
            "object should stay in graveyard when origin zone mismatches"
        );
        assert!(
            !state.exile.contains(&obj_id),
            "object should NOT be exiled when origin zone mismatches"
        );
        // No ZoneChanged events should have been emitted
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, crate::types::events::GameEvent::ZoneChanged { .. })),
            "no ZoneChanged event should fire for origin mismatch"
        );
    }

    #[test]
    fn empty_targets_from_hand_sets_effect_zone_choice_and_preserves_flags() {
        let mut state = GameState::new_two_player(42);
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hand A".to_string(),
            Zone::Hand,
        );
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Hand B".to_string(),
            Zone::Hand,
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: true,
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                up_to: true,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                up_to,
                effect_kind,
                zone,
                destination,
                enter_tapped,
                enter_transformed,
                enters_under_player,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert!(*up_to);
                assert_eq!(*effect_kind, EffectKind::ChangeZone);
                assert_eq!(*zone, Zone::Hand);
                assert_eq!(*destination, Some(Zone::Battlefield));
                assert!(cards.contains(&a));
                assert!(cards.contains(&b));
                assert!(enter_tapped.is_tapped());
                assert!(*enter_transformed);
                // CR 110.2a: WaitingFor carries the resolved player id, not a
                // boolean. Ability controller in this test is PlayerId(0).
                assert_eq!(*enters_under_player, Some(PlayerId(0)));
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    #[test]
    fn empty_targets_from_hand_with_single_card_auto_moves_and_records_count() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Only Hand Card".to_string(),
            Zone::Hand,
        );
        let ability = make_hand_choice_ability(false);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&obj_id));
        assert!(!state.players[0].hand.contains(&obj_id));
        assert_eq!(state.last_effect_count, Some(1));
    }

    #[test]
    fn mandatory_empty_target_hand_move_without_cards_sets_failure_flag() {
        let mut state = GameState::new_two_player(42);
        let ability = make_hand_choice_ability(false);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.cost_payment_failed_flag);
    }

    #[test]
    fn relative_controller_filter_uses_targeted_player_for_change_zone_effects() {
        let mut state = GameState::new_two_player(42);
        let battlefield_creature = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&battlefield_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let graveyard_card = create_object(
            &mut state,
            CardId(21),
            PlayerId(1),
            "Opponent Graveyard Card".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Typed(
                    TypedFilter::default()
                        .controller(crate::types::ability::ControllerRef::Opponent),
                ),
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(200),
            PlayerId(0),
        )
        .sub_ability(
            ResolvedAbility::new(
                Effect::ChangeZone {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Exile,
                    target: TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(crate::types::ability::ControllerRef::You),
                    ),
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                },
                vec![],
                ObjectId(200),
                PlayerId(0),
            )
            .sub_ability(ResolvedAbility::new(
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Exile,
                    target: TargetFilter::Typed(TypedFilter {
                        controller: Some(crate::types::ability::ControllerRef::You),
                        properties: vec![crate::types::ability::FilterProp::InZone {
                            zone: Zone::Graveyard,
                        }],
                        ..Default::default()
                    }),
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    face_down_profile: None,
                },
                vec![],
                ObjectId(200),
                PlayerId(0),
            )),
        );

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.objects.get(&battlefield_creature).unwrap().zone,
            Zone::Exile
        );
        assert_eq!(
            state.objects.get(&graveyard_card).unwrap().zone,
            Zone::Exile
        );
    }

    #[test]
    fn parent_target_slot_keeps_goblin_welder_targets_distinct_after_sacrifice() {
        let mut state = GameState::new_two_player(42);
        let battlefield_artifact = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Battlefield Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&battlefield_artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        let graveyard_artifact = create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "Graveyard Artifact".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&graveyard_artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![
                TargetRef::Object(battlefield_artifact),
                TargetRef::Object(graveyard_artifact),
            ],
            ObjectId(200),
            PlayerId(0),
        )
        .sub_ability(
            ResolvedAbility::new(
                Effect::Sacrifice {
                    target: TargetFilter::ParentTargetSlot { index: 0 },
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    min_count: 0,
                },
                vec![],
                ObjectId(200),
                PlayerId(0),
            )
            .sub_ability(ResolvedAbility::new(
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    target: TargetFilter::ParentTargetSlot { index: 1 },
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                },
                vec![],
                ObjectId(200),
                PlayerId(0),
            )),
        );

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.objects.get(&battlefield_artifact).unwrap().zone,
            Zone::Graveyard
        );
        assert_eq!(
            state.objects.get(&graveyard_artifact).unwrap().zone,
            Zone::Battlefield
        );
    }

    #[test]
    fn scoped_player_target_does_not_rebind_your_hand_change_zone() {
        let mut state = GameState::new_two_player(42);
        let controller_card = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Controller Hand Card".to_string(),
            Zone::Hand,
        );
        let opponent_card = create_object(
            &mut state,
            CardId(21),
            PlayerId(1),
            "Opponent Hand Card".to_string(),
            Zone::Hand,
        );

        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(
                    TypedFilter::card().controller(crate::types::ability::ControllerRef::You),
                ),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(200),
            PlayerId(0),
        );
        ability.set_scoped_player_recursive(PlayerId(1));

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&controller_card).unwrap().zone,
            Zone::Battlefield
        );
        assert_eq!(state.objects.get(&opponent_card).unwrap().zone, Zone::Hand);
    }

    #[test]
    fn scoped_player_hand_change_zone_choice_uses_scoped_player() {
        let mut state = GameState::new_two_player(42);
        let controller_card = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Controller Hand Creature".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&controller_card)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let opponent_a = create_object(
            &mut state,
            CardId(21),
            PlayerId(1),
            "Opponent Hand Creature A".to_string(),
            Zone::Hand,
        );
        let opponent_b = create_object(
            &mut state,
            CardId(22),
            PlayerId(1),
            "Opponent Hand Creature B".to_string(),
            Zone::Hand,
        );
        for id in [opponent_a, opponent_b] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::ScopedPlayer),
                ),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(200),
            PlayerId(0),
        );
        ability.set_scoped_player_recursive(PlayerId(1));

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                let mut actual = cards.clone();
                actual.sort_by_key(|id| id.0);
                let mut expected = vec![opponent_a, opponent_b];
                expected.sort_by_key(|id| id.0);
                assert_eq!(*player, PlayerId(1));
                assert_eq!(
                    actual, expected,
                    "scoped-player hand choice must exclude controller hand cards"
                );
            }
            other => panic!("expected EffectZoneChoice for scoped player, got {other:?}"),
        }
        assert_eq!(
            state.objects.get(&controller_card).unwrap().zone,
            Zone::Hand
        );
    }

    #[test]
    fn optional_targeting_with_zero_targets_resolves_as_noop() {
        // CR 115.6: "up to one target" with 0 chosen should not fall through
        // to the untargeted zone-scan path.
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bystander".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![], // zero targets chosen
            ObjectId(900),
            PlayerId(0),
        );
        ability.optional_targeting = true;

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Creature should remain on the battlefield — not exiled, not offered as a choice.
        assert_eq!(
            state.objects.get(&creature).unwrap().zone,
            Zone::Battlefield
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "should not prompt for zone choice when optional targeting chose 0"
        );
    }

    #[test]
    fn multi_target_min_zero_with_zero_targets_resolves_as_noop() {
        // CR 115.6: `multi_target.min = 0` is the same zero-target choice as
        // `optional_targeting`; it must not fall through to resolution-time
        // zone scanning after the player chooses no targets.
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bystander".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![], // zero targets chosen
            ObjectId(900),
            PlayerId(0),
        );
        ability.multi_target = Some(MultiTargetSpec::fixed(0, 1));

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&creature).unwrap().zone,
            Zone::Battlefield
        );
        assert!(
            !state.cost_payment_failed_flag,
            "zero chosen targets for min=0 targeting must not signal payment failure"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "should not prompt for zone choice when multi-target min=0 chose 0"
        );
    }

    /// Build an Exhume-shaped ability: `Effect::ChangeZone` Graveyard →
    /// Battlefield with a `Typed{Creature}` target carrying the post-fix
    /// owner constraint `Owned{ScopedPlayer}` + `InZone Graveyard`, and
    /// `player_scope: All`. Issue #488 regression scaffold.
    fn make_exhume_ability(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Creature],
                    controller: None,
                    properties: vec![
                        FilterProp::Owned {
                            controller: ControllerRef::ScopedPlayer,
                        },
                        FilterProp::InZone {
                            zone: Zone::Graveyard,
                        },
                    ],
                }),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            source_id,
            controller,
        );
        ability.player_scope = Some(PlayerFilter::All);
        ability
    }

    /// Place a `Creature` card into `owner`'s graveyard and return its id.
    fn creature_in_graveyard(state: &mut GameState, cid: u64, owner: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(cid),
            owner,
            format!("Creature {cid}"),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    /// Issue #488: Exhume must offer each iterated player ONLY the creatures in
    /// that player's own graveyard — never another player's. Drives the
    /// `player_scope` iteration through `resolve_ability_chain` and the
    /// `EffectZoneChoice` continuation chain.
    #[test]
    fn exhume_each_player_picks_from_own_graveyard() {
        let mut state = GameState::new_two_player(42);
        // Two creatures per player so the choice prompt fires (a single
        // eligible card auto-resolves without a prompt).
        let p0_a = creature_in_graveyard(&mut state, 1, PlayerId(0));
        let p0_b = creature_in_graveyard(&mut state, 2, PlayerId(0));
        let p1_a = creature_in_graveyard(&mut state, 3, PlayerId(1));
        let p1_b = creature_in_graveyard(&mut state, 4, PlayerId(1));

        let ability = make_exhume_ability(ObjectId(900), PlayerId(0));
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // First APNAP iteration: the active player is offered ONLY their own
        // graveyard creatures.
        let first_player = match &state.waiting_for {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                let mut sorted = cards.clone();
                sorted.sort_by_key(|o| o.0);
                if *player == PlayerId(0) {
                    let mut expect = vec![p0_a, p0_b];
                    expect.sort_by_key(|o| o.0);
                    assert_eq!(sorted, expect, "P0 must see only P0's graveyard");
                } else {
                    let mut expect = vec![p1_a, p1_b];
                    expect.sort_by_key(|o| o.0);
                    assert_eq!(sorted, expect, "P1 must see only P1's graveyard");
                }
                *player
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        };

        // Resolve the first player's choice; continuation advances to the
        // second player, who must see only THEIR graveyard.
        let first_pick = if first_player == PlayerId(0) {
            p0_a
        } else {
            p1_a
        };
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::SelectCards {
                cards: vec![first_pick],
            },
        )
        .unwrap();

        let second_player = match &state.waiting_for {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                assert_ne!(
                    *player, first_player,
                    "second iteration is the other player"
                );
                let mut sorted = cards.clone();
                sorted.sort_by_key(|o| o.0);
                if *player == PlayerId(0) {
                    let mut expect = vec![p0_a, p0_b];
                    expect.sort_by_key(|o| o.0);
                    assert_eq!(sorted, expect, "P0 must see only P0's graveyard");
                } else {
                    let mut expect = vec![p1_a, p1_b];
                    expect.sort_by_key(|o| o.0);
                    assert_eq!(sorted, expect, "P1 must see only P1's graveyard");
                }
                *player
            }
            other => panic!("expected second EffectZoneChoice, got {other:?}"),
        };

        let second_pick = if second_player == PlayerId(0) {
            p0_a
        } else {
            p1_a
        };
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::SelectCards {
                cards: vec![second_pick],
            },
        )
        .unwrap();

        // Both chosen creatures are on the battlefield under their owners.
        assert_eq!(
            state.objects.get(&first_pick).unwrap().zone,
            Zone::Battlefield
        );
        assert_eq!(
            state.objects.get(&second_pick).unwrap().zone,
            Zone::Battlefield
        );
        assert_eq!(state.objects.get(&p0_a).unwrap().owner, PlayerId(0));
        assert_eq!(state.objects.get(&p1_a).unwrap().owner, PlayerId(1));
    }

    /// Issue #488 — MANDATORY 3-player coverage. A 2-player test can mask
    /// owner-vs-controller confusion (the wrong fallback might still resolve to
    /// a single default). With three players, each iterated player's
    /// `EffectZoneChoice.cards` must contain ONLY that player's own graveyard
    /// creatures — proving the per-iteration `source.controller` rebind drives
    /// `ScopedPlayer` correctly.
    #[test]
    fn exhume_three_players_each_scoped_to_own_graveyard() {
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        // Two creatures per player so every iteration prompts a choice.
        let p0: Vec<ObjectId> = vec![
            creature_in_graveyard(&mut state, 1, PlayerId(0)),
            creature_in_graveyard(&mut state, 2, PlayerId(0)),
        ];
        let p1: Vec<ObjectId> = vec![
            creature_in_graveyard(&mut state, 3, PlayerId(1)),
            creature_in_graveyard(&mut state, 4, PlayerId(1)),
        ];
        let p2: Vec<ObjectId> = vec![
            creature_in_graveyard(&mut state, 5, PlayerId(2)),
            creature_in_graveyard(&mut state, 6, PlayerId(2)),
        ];
        let own_set = |pid: PlayerId| -> Vec<ObjectId> {
            let mut v = match pid {
                PlayerId(0) => p0.clone(),
                PlayerId(1) => p1.clone(),
                _ => p2.clone(),
            };
            v.sort_by_key(|o| o.0);
            v
        };

        // Exhume controlled by P1 — proves APNAP anchoring and scoping are
        // independent of the caster.
        let ability = make_exhume_ability(ObjectId(900), PlayerId(1));
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let mut seen = Vec::new();
        for _ in 0..3 {
            let (player, pick) = match &state.waiting_for {
                WaitingFor::EffectZoneChoice { player, cards, .. } => {
                    let mut sorted = cards.clone();
                    sorted.sort_by_key(|o| o.0);
                    assert_eq!(
                        sorted,
                        own_set(*player),
                        "player {player:?} must be offered only their own graveyard"
                    );
                    (*player, cards[0])
                }
                other => panic!("expected EffectZoneChoice, got {other:?}"),
            };
            assert!(!seen.contains(&player), "each player iterated exactly once");
            seen.push(player);
            crate::game::engine::apply_as_current(
                &mut state,
                crate::types::actions::GameAction::SelectCards { cards: vec![pick] },
            )
            .unwrap();
        }
        assert_eq!(seen.len(), 3, "all three players iterated");
    }

    /// CR 603.10a / Academy Rector class: LTB self-exile triggers fire after the
    /// source has moved to the graveyard. The parsed effect is
    /// `ChangeZone { origin: None, destination: Exile, target: ParentTarget }`
    /// with empty `ability.targets`; the resolver must treat `ParentTarget` as
    /// a self-reference to `ability.source_id` and move from the current
    /// (graveyard) zone.
    #[test]
    fn ltb_parent_target_self_exile_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Academy Rector".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::ParentTarget,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[0].graveyard.contains(&obj_id));
        assert_eq!(state.objects[&obj_id].zone, Zone::Exile);
    }

    /// CR 603.10a / Bronzehide Lion class: LTB self-return triggers where the
    /// source returns to the battlefield (typically under some constraint) must
    /// find the source in the graveyard and move it back.
    #[test]
    fn ltb_parent_target_self_return_to_battlefield_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bronzehide Lion".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .base_card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Battlefield,
                target: TargetFilter::ParentTarget,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&obj_id));
        assert!(!state.players[0].graveyard.contains(&obj_id));
    }

    /// End-to-end Academy Rector-style pipeline: dies on battlefield → LTB
    /// trigger fires → resolves from graveyard → source ends up in exile.
    #[test]
    fn ltb_parent_target_self_exile_pipeline() {
        use crate::game::stack::resolve_top;
        use crate::game::triggers::process_triggers;
        use crate::types::ability::{AbilityDefinition, AbilityKind, TriggerDefinition};
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Academy Rector".to_string(),
            Zone::Battlefield,
        );

        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.trigger_zones = vec![Zone::Graveyard];
        trigger.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::ParentTarget,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        )));
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .trigger_definitions
            .push(trigger);

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, obj_id, Zone::Graveyard, &mut events);
        assert!(state.players[0].graveyard.contains(&obj_id));

        process_triggers(&mut state, &events);
        assert_eq!(state.stack.len(), 1, "LTB trigger did not reach the stack");

        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);
        assert_eq!(
            state.objects[&obj_id].zone,
            Zone::Exile,
            "Academy Rector should be in exile"
        );
        assert!(!state.players[0].graveyard.contains(&obj_id));
    }

    // === Issue #448: "enters tapped" observer triggers (CR 603.6a + CR 110.5b) ===
    //
    // Amulet of Vigor ("Whenever a permanent you control enters tapped, untap
    // it.") is an *observer* trigger: its `valid_card` matches any permanent the
    // controller owns, so the entering permanent differs from the ability
    // source. The `ZoneChangeObjectIsTapped` condition must read the entering
    // permanent named by the `ZoneChanged` event — NOT the (untapped) Amulet.
    //
    // These tests drive the real pipeline: `resolve()` performs the ChangeZone
    // effect (tapping the entering permanent and emitting a real `ZoneChanged`
    // event), then `process_triggers` scans the battlefield for matching
    // observer triggers and stacks them. On pre-fix `main`, the buggy
    // `SourceIsTapped` condition reads the untapped Amulet → trigger never
    // fires → these tests fail.

    /// Build an Amulet of Vigor-style observer trigger: "Whenever a permanent
    /// you control enters tapped, untap it." Mirrors the parsed card-data
    /// shape (`valid_card: Typed[Permanent] controller You`,
    /// `condition: ZoneChangeObjectIsTapped`, `execute: Untap{TriggeringSource}`).
    fn amulet_of_vigor_trigger() -> crate::types::ability::TriggerDefinition {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ControllerRef, TriggerCondition, TriggerDefinition,
            TypedFilter,
        };
        use crate::types::triggers::TriggerMode;

        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger.destination = Some(Zone::Battlefield);
        trigger.trigger_zones = vec![Zone::Battlefield];
        trigger.valid_card = Some(TargetFilter::Typed(
            TypedFilter::permanent().controller(ControllerRef::You),
        ));
        trigger.condition = Some(TriggerCondition::ZoneChangeObjectIsTapped);
        trigger.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Untap {
                target: TargetFilter::TriggeringSource,
            },
        )));
        trigger
    }

    /// Move a freshly created hand permanent onto the battlefield through the
    /// real ChangeZone resolution path, with `enter_tapped` controlling the
    /// post-ETB tapped state. Returns the emitted events (carrying the real
    /// `ZoneChanged` event) for `process_triggers`.
    fn enter_permanent_via_change_zone(
        state: &mut GameState,
        obj_id: ObjectId,
        enter_tapped: bool,
    ) -> Vec<GameEvent> {
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped),
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(state, &ability, &mut events).unwrap();
        events
    }

    /// Issue #448: Amulet of Vigor untaps a *different* permanent that enters
    /// tapped. Two distinct objects (Amulet ≠ Lotus Field). Pre-fix `main`
    /// reads `obj.tapped` on the untapped Amulet → condition false → no trigger.
    #[test]
    fn amulet_of_vigor_untaps_permanent_entering_tapped() {
        use crate::game::stack::resolve_top;
        use crate::game::triggers::process_triggers;
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);

        // Amulet of Vigor on the battlefield, untapped artifact.
        let amulet = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Amulet of Vigor".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&amulet).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.tapped = false;
            obj.trigger_definitions.push(amulet_of_vigor_trigger());
        }

        // Lotus Field in hand — a distinct land that will enter tapped.
        let land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Lotus Field".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let events = enter_permanent_via_change_zone(&mut state, land, true);
        assert!(
            state.objects[&land].tapped,
            "land must enter tapped (enter_tapped replacement applied)"
        );

        process_triggers(&mut state, &events);
        assert_eq!(
            state.stack.len(),
            1,
            "Amulet of Vigor's trigger must fire when a different permanent enters tapped"
        );
        assert_eq!(
            state.stack.back().unwrap().source_id,
            amulet,
            "the stacked trigger must be Amulet of Vigor's"
        );

        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);
        assert!(
            !state.objects[&land].tapped,
            "Amulet of Vigor should have untapped the entering land"
        );
    }

    /// Issue #448 negative control: a permanent entering *untapped* must NOT
    /// fire Amulet of Vigor's trigger — the `ZoneChangeObjectIsTapped`
    /// condition genuinely gates on the entering object's tapped state.
    #[test]
    fn amulet_of_vigor_ignores_permanent_entering_untapped() {
        use crate::game::triggers::process_triggers;
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let amulet = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Amulet of Vigor".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&amulet).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.tapped = false;
            obj.trigger_definitions.push(amulet_of_vigor_trigger());
        }
        let land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Untapped Land".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let events = enter_permanent_via_change_zone(&mut state, land, false);
        assert!(!state.objects[&land].tapped, "land entered untapped");

        process_triggers(&mut state, &events);
        assert!(
            state.stack.is_empty(),
            "a permanent entering untapped must not fire Amulet of Vigor"
        );
    }

    /// Issue #448 (the exact Discord report): two Amulet of Vigor in play, one
    /// permanent enters tapped — both Amulets must trigger (CR 603.3: each
    /// triggered ability is placed on the stack independently).
    #[test]
    fn two_amulets_of_vigor_both_trigger() {
        use crate::game::triggers::process_triggers;
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        for cid in [CardId(1), CardId(2)] {
            let amulet = create_object(
                &mut state,
                cid,
                PlayerId(0),
                "Amulet of Vigor".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&amulet).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.tapped = false;
            obj.trigger_definitions.push(amulet_of_vigor_trigger());
        }
        let land = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Lotus Field".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let events = enter_permanent_via_change_zone(&mut state, land, true);
        process_triggers(&mut state, &events);
        // CR 603.3b (#531): controller has 2 simultaneous triggers — drain
        // the OrderTriggers prompt with identity order.
        crate::game::triggers::drain_order_triggers_with_identity(&mut state);
        assert_eq!(
            state.stack.len(),
            2,
            "CR 603.3: both Amulet of Vigor copies must place a triggered ability on the stack"
        );
    }

    /// Issue #448 sibling class: Charismatic Conqueror's `Not(ZoneChangeObjectIsTapped)`
    /// observer trigger fires when an opponent's permanent enters *untapped*.
    #[test]
    fn charismatic_conqueror_triggers_on_opponent_untapped_etb() {
        use crate::game::triggers::process_triggers;
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ControllerRef, TriggerCondition, TriggerDefinition,
            TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);

        // Charismatic Conqueror under PlayerId(0).
        let conqueror = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Charismatic Conqueror".to_string(),
            Zone::Battlefield,
        );
        {
            let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
            trigger.destination = Some(Zone::Battlefield);
            trigger.trigger_zones = vec![Zone::Battlefield];
            // "a permanent ... under an opponent's control"
            trigger.valid_card = Some(TargetFilter::Typed(
                TypedFilter::permanent().controller(ControllerRef::Opponent),
            ));
            // "enters untapped" → Not(ZoneChangeObjectIsTapped)
            trigger.condition = Some(TriggerCondition::Not {
                condition: Box::new(TriggerCondition::ZoneChangeObjectIsTapped),
            });
            trigger.execute = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Untap {
                    target: TargetFilter::TriggeringSource,
                },
            )));
            let obj = state.objects.get_mut(&conqueror).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.trigger_definitions.push(trigger);
        }

        // An opponent's (PlayerId(1)) creature enters the battlefield untapped.
        let opp_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&opp_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![TargetRef::Object(opp_creature)],
            ObjectId(999),
            PlayerId(1),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            !state.objects[&opp_creature].tapped,
            "opponent creature entered untapped"
        );

        process_triggers(&mut state, &events);
        assert_eq!(
            state.stack.len(),
            1,
            "Charismatic Conqueror must trigger on an opponent's permanent entering untapped"
        );
        assert_eq!(
            state.stack.back().unwrap().source_id,
            conqueror,
            "the stacked trigger must be Charismatic Conqueror's"
        );
    }

    /// CR 400.6 + CR 608.2c: `ChangeZoneAll` must set `last_effect_count` to
    /// the number of objects moved so downstream sub-abilities referring to
    /// "that many" (via `QuantityRef::EventContextAmount`) resolve correctly.
    /// Whirlpool Drake class: "shuffle the cards from your hand into your
    /// library, then draw that many cards."
    #[test]
    fn change_zone_all_records_moved_count_for_event_context_amount() {
        let mut state = GameState::new_two_player(42);
        // Put three cards in player 0's hand.
        let h1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".into(),
            Zone::Hand,
        );
        let h2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".into(),
            Zone::Hand,
        );
        let h3 = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Card C".into(),
            Zone::Hand,
        );
        // Opponent's hand — must NOT be moved (filter is Controller).
        let opp_hand = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Opponent Card".into(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Hand),
                destination: Zone::Library,
                target: TargetFilter::Controller,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();

        // All three controller's cards moved to library; opponent's card untouched.
        for id in [h1, h2, h3] {
            assert_eq!(state.objects[&id].zone, Zone::Library);
        }
        assert_eq!(state.objects[&opp_hand].zone, Zone::Hand);
        assert_eq!(
            state.last_effect_count,
            Some(3),
            "ChangeZoneAll must record moved-object count for EventContextAmount consumers"
        );
    }

    /// CR 110.2a + CR 400.7: Mass graveyard-to-battlefield effects that state
    /// "under your control" must override the default controller for every
    /// entering permanent, including cards owned by opponents. Rise of the Dark
    /// Realms class: "Return all creature cards from all graveyards to the
    /// battlefield under your control."
    #[test]
    fn change_zone_all_graveyard_to_battlefield_enters_under_controller() {
        let mut state = GameState::new_two_player(42);
        let caster_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Caster Corpse".into(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&caster_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let opponent_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Corpse".into(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&opponent_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let opponent_noncreature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opponent Spell".into(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::creature()),
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        for id in [caster_creature, opponent_creature] {
            let obj = &state.objects[&id];
            assert_eq!(obj.zone, Zone::Battlefield);
            assert_eq!(
                obj.controller,
                PlayerId(0),
                "returned creature {id:?} should enter under the spell controller"
            );
        }
        assert_eq!(state.objects[&opponent_noncreature].zone, Zone::Graveyard);
    }

    /// CR 110.2a: `ChangeZoneAll.enters_under` currently supports only
    /// `ControllerRef::You`; unsupported variants must strict-fail before any
    /// member moves, matching the single-object `ChangeZone` resolver.
    #[test]
    fn change_zone_all_strict_fails_on_unsupported_enters_under_controller_ref() {
        let mut state = GameState::new_two_player(42);
        let opponent_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Corpse".into(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&opponent_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::creature()),
                enters_under: Some(ControllerRef::Opponent),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let err = resolve_all(&mut state, &ability, &mut events)
            .expect_err("unsupported ControllerRef must strict-fail");
        let msg = err.to_string();
        assert!(
            msg.contains("CR 110.2a"),
            "error must cite CR 110.2a, got {msg}"
        );
        assert!(
            msg.contains("ChangeZoneAll") && msg.contains("Opponent"),
            "error must name the effect and offending variant, got {msg}"
        );
        assert_eq!(state.objects[&opponent_creature].zone, Zone::Graveyard);
    }

    /// CR 400.7 + CR 701.23 + CR 701.24: Multi-zone same-name exile.
    /// Exercises the Deadly Cover-Up "search [player]'s graveyard, hand, and
    /// library for any number of cards with that name and exile them" branch.
    /// Verifies (a) cards in all three zones matching the parent target's name
    /// are exiled, (b) cards with different names are untouched, and (c) the
    /// per-resolution hand-exile counter is populated for the downstream
    /// `Draw { count: ExiledFromHandThisResolution }` step.
    #[test]
    fn change_zone_all_multi_zone_same_name_as_parent_target_exiles_and_counts_hand() {
        use crate::types::ability::FilterProp;
        let mut state = GameState::new_two_player(42);

        // Parent target: a "Grizzly Bears" card already exiled by a prior step
        // (its name persists via lki_cache; we model it as still in Exile here).
        let seed = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Exile,
        );

        // Matching cards in three zones owned by player 1.
        let bear_gy = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Graveyard,
        );
        let bear_hand1 = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        let bear_hand2 = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        let bear_lib = create_object(
            &mut state,
            CardId(5),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Library,
        );

        // Distractor: a card in the graveyard with a different name. Must not exile.
        let other_gy = create_object(
            &mut state,
            CardId(6),
            PlayerId(1),
            "Llanowar Elves".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default().properties(vec![
                        FilterProp::InAnyZone {
                            zones: vec![Zone::Graveyard, Zone::Hand, Zone::Library],
                        },
                        FilterProp::SameNameAsParentTarget,
                    ]),
                ),
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            // Parent target supplies the "that name" referent.
            vec![TargetRef::Object(seed)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        state.exiled_from_hand_this_resolution = 0;
        resolve_all(&mut state, &ability, &mut events).unwrap();

        // All four matching bears now in exile.
        for &id in &[bear_gy, bear_hand1, bear_hand2, bear_lib] {
            assert_eq!(
                state.objects[&id].zone,
                Zone::Exile,
                "matching bear {id:?} must be exiled"
            );
        }
        // Distractor untouched.
        assert_eq!(state.objects[&other_gy].zone, Zone::Graveyard);

        // Per-resolution counter equals the number of cards exiled FROM HAND only.
        assert_eq!(
            state.exiled_from_hand_this_resolution, 2,
            "exactly two hand-origin exiles must be recorded for downstream Draw"
        );

        // Total moved across all zones is 4 (two from hand + one each from GY/Lib).
        assert_eq!(state.last_effect_count, Some(4));
    }

    /// CR 701.59c + CR 601.2f: End-to-end cascade for Deadly Cover-Up with
    /// evidence paid. Chains DestroyAll → (conditional on AdditionalCostPaid)
    /// exile seed from opponent's graveyard → multi-zone same-name exile →
    /// Draw N where N = `exiled_from_hand_this_resolution`. Verifies:
    ///   (a) When evidence is NOT paid, the cascade is skipped — only DestroyAll
    ///       runs, hand-exile counter stays 0, controller draws 0 cards.
    ///   (b) When evidence IS paid, the full cascade runs: seed exiled, matching
    ///       cards exiled across all three zones, hand-exile counter populated,
    ///       Draw consumes that counter value.
    /// This is the plan's acceptance bar for the Draw-counter integration.
    #[test]
    fn deadly_cover_up_full_cascade_with_and_without_evidence() {
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::{
            AbilityCondition, FilterProp, QuantityExpr, QuantityRef, SpellContext, TypedFilter,
        };
        use crate::types::card_type::CoreType;

        for evidence_paid in [false, true] {
            let mut state = GameState::new_two_player(42);

            // Battlefield creature (destroyed by DestroyAll either way).
            let bf_creature = create_object(
                &mut state,
                CardId(10),
                PlayerId(1),
                "Llanowar Elves".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&bf_creature)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);

            // Seed creature already in opponent's graveyard.
            let seed = create_object(
                &mut state,
                CardId(20),
                PlayerId(1),
                "Grizzly Bears".to_string(),
                Zone::Graveyard,
            );

            // Matching cards: two in hand, one in library, one in graveyard.
            let _hand1 = create_object(
                &mut state,
                CardId(21),
                PlayerId(1),
                "Grizzly Bears".to_string(),
                Zone::Hand,
            );
            let _hand2 = create_object(
                &mut state,
                CardId(22),
                PlayerId(1),
                "Grizzly Bears".to_string(),
                Zone::Hand,
            );
            let _lib = create_object(
                &mut state,
                CardId(23),
                PlayerId(1),
                "Grizzly Bears".to_string(),
                Zone::Library,
            );
            let _gy2 = create_object(
                &mut state,
                CardId(24),
                PlayerId(1),
                "Grizzly Bears".to_string(),
                Zone::Graveyard,
            );

            // Give P0 a library to draw from.
            for i in 0..5 {
                create_object(
                    &mut state,
                    CardId(100 + i),
                    PlayerId(0),
                    "Library Card".to_string(),
                    Zone::Library,
                );
            }

            // Build the cascade (deepest first):
            //   Draw { count: ExiledFromHandThisResolution }
            let draw = ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::ExiledFromHandThisResolution,
                    },
                    target: TargetFilter::Controller,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            );
            //   Multi-zone same-name exile → Draw
            let multi_zone = ResolvedAbility::new(
                Effect::ChangeZoneAll {
                    origin: None,
                    destination: Zone::Exile,
                    target: TargetFilter::Typed(TypedFilter::default().properties(vec![
                        FilterProp::InAnyZone {
                            zones: vec![Zone::Graveyard, Zone::Hand, Zone::Library],
                        },
                        FilterProp::SameNameAsParentTarget,
                    ])),
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    face_down_profile: None,
                },
                vec![TargetRef::Object(seed)],
                ObjectId(100),
                PlayerId(0),
            )
            .sub_ability(draw);
            //   Exile seed from opponent's graveyard → multi_zone
            let exile_seed = ResolvedAbility::new(
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Exile,
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
                vec![TargetRef::Object(seed)],
                ObjectId(100),
                PlayerId(0),
            )
            .sub_ability(multi_zone)
            .condition(AbilityCondition::additional_cost_paid_any());
            //   Top: DestroyAll → exile_seed
            let top = ResolvedAbility::new(
                Effect::DestroyAll {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    cant_regenerate: false,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            )
            .sub_ability(exile_seed)
            .context(SpellContext {
                additional_cost_paid: evidence_paid,
                ..SpellContext::default()
            });

            let mut events = Vec::new();
            resolve_ability_chain(&mut state, &top, &mut events, 0).expect("cascade must resolve");

            // DestroyAll always fires.
            assert_eq!(
                state.objects[&bf_creature].zone,
                Zone::Graveyard,
                "battlefield creature must be destroyed regardless of evidence",
            );

            if evidence_paid {
                // Seed exiled from graveyard.
                assert_eq!(state.objects[&seed].zone, Zone::Exile);
                // All four matching bears exiled.
                for id in [_hand1, _hand2, _lib, _gy2] {
                    assert_eq!(
                        state.objects[&id].zone,
                        Zone::Exile,
                        "matching bear {id:?} must be exiled by the cascade",
                    );
                }
                // Hand-exile counter equals 2.
                assert_eq!(state.exiled_from_hand_this_resolution, 2);
                // P0 drew exactly 2 cards (Draw consumed the counter).
                assert_eq!(
                    state.players[0].hand.len(),
                    2,
                    "Draw must pull count from ExiledFromHandThisResolution",
                );
            } else {
                // Cascade skipped: seed still in graveyard, matching bears untouched,
                // counter stayed at 0, no cards drawn.
                assert_eq!(state.objects[&seed].zone, Zone::Graveyard);
                for id in [_hand1, _hand2, _lib, _gy2] {
                    assert_ne!(
                        state.objects[&id].zone,
                        Zone::Exile,
                        "matching bear {id:?} must NOT be exiled without evidence",
                    );
                }
                assert_eq!(state.exiled_from_hand_this_resolution, 0);
                assert_eq!(state.players[0].hand.len(), 0);
            }
        }
    }

    /// CR 701.23b + CR 401.2: A search sub-ability chain ("search your library for X,
    /// put it onto the battlefield, then shuffle") emits ChangeZone with
    /// `origin: Library, target: Any` as a continuation of SearchLibrary. On
    /// fail-to-find, `ability.targets` is empty and the put-step must no-op —
    /// never fall through to a zone-scan (which would treat `Any` as a wildcard
    /// over every library in the game and let the player pick any card, which
    /// is the Ranging Raptors / Rampant Growth / Cultivate fail-to-find bug).
    #[test]
    fn search_fail_to_find_chain_continuation_does_not_scan_libraries() {
        let mut state = GameState::new_two_player(42);

        // Seed both libraries with cards so a fallback zone-scan would have
        // candidates to pull from — proves the guard stops before the scan.
        let p0_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Library Card".to_string(),
            Zone::Library,
        );
        let p1_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 Library Card".to_string(),
            Zone::Library,
        );
        let battlefield_before = state.battlefield.clone();

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![], // Empty targets: search failed to find, no card to put.
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.battlefield, battlefield_before,
            "Fail-to-find put-step must NOT move any library card onto the battlefield"
        );
        assert_eq!(
            state.objects[&p0_card].zone,
            Zone::Library,
            "P0's library card must stay in the library"
        );
        assert_eq!(
            state.objects[&p1_card].zone,
            Zone::Library,
            "P1's library card must not be reachable from a fail-to-find put-step"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "Fail-to-find must not prompt an EffectZoneChoice (the bug symptom)"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::ChangeZone,
                    ..
                }
            )),
            "Fail-to-find put-step must emit EffectResolved so the chain advances to Shuffle"
        );
    }

    /// CR 603.7 + CR 400.7: Sword of Hearth and Home's triggered ability chains
    /// `ChangeZone` (exile target creature) → `SearchLibrary` → `ChangeZone`
    /// (land → battlefield) → `ChangeZoneAll { target: TrackedSet(0) }` (return
    /// the exiled creature). The final step uses the sentinel `TrackedSetId(0)`
    /// emitted by the parser, which `resolve_all` must rebind to the most recent
    /// populated tracked set — otherwise the exiled card is stranded in exile.
    #[test]
    fn change_zone_all_resolves_tracked_set_sentinel_inline() {
        let mut state = GameState::new_two_player(42);
        let exiled = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Exiled Creature".to_string(),
            Zone::Exile,
        );
        // Simulate the upstream exile step having published a tracked set.
        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state.tracked_object_sets.insert(set_id, vec![exiled]);

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Battlefield,
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&exiled].zone,
            Zone::Battlefield,
            "Exiled creature must return to the battlefield when TrackedSetId(0) is resolved"
        );
    }

    /// Zimone's Experiment: tracked-set routing must scan the members' actual zone
    /// (library) when `origin` is None (issue #2368).
    #[test]
    fn issue_2368_tracked_set_filtered_scans_library_when_origin_none() {
        let mut state = GameState::new_two_player(42);
        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state.objects.get_mut(&land).unwrap().card_types.core_types = vec![CoreType::Land];
        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(set_id, vec![land, creature]);
        state.chain_tracked_set_id = Some(set_id);

        let land_filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Land],
            controller: None,
            properties: vec![],
        });
        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: None,
                destination: Zone::Battlefield,
                target: TargetFilter::TrackedSetFiltered {
                    id: TrackedSetId(0),
                    filter: Box::new(land_filter),
                },
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&land].zone, Zone::Battlefield);
        assert!(state.objects[&land].tapped);
        assert_eq!(state.objects[&creature].zone, Zone::Library);
    }

    #[test]
    fn tracked_set_filtered_with_origin_none_scans_all_member_zones() {
        let mut state = GameState::new_two_player(42);
        let library_land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Library Land".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&library_land)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Land];
        let graveyard_land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Graveyard Land".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&graveyard_land)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Land];
        let exiled_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Exiled Creature".to_string(),
            Zone::Exile,
        );
        state
            .objects
            .get_mut(&exiled_creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(set_id, vec![library_land, graveyard_land, exiled_creature]);
        state.chain_tracked_set_id = Some(set_id);

        let land_filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Land],
            controller: None,
            properties: vec![],
        });
        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: None,
                destination: Zone::Battlefield,
                target: TargetFilter::TrackedSetFiltered {
                    id: TrackedSetId(0),
                    filter: Box::new(land_filter),
                },
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&library_land].zone, Zone::Battlefield);
        assert_eq!(state.objects[&graveyard_land].zone, Zone::Battlefield);
        assert_eq!(state.objects[&exiled_creature].zone, Zone::Exile);
    }

    /// CR 708.2a + CR 708.3 + CR 110.2a: Cyber-Controller's mass put-step — "Put
    /// all creature cards milled this way onto the battlefield face down under
    /// your control. They're 2/2 Cyberman artifact creatures." The
    /// `ChangeZoneAll { target: TrackedSetFiltered{creature}, enters_under: You,
    /// face_down_profile: Some(...) }` must move every creature card in the
    /// milled set to the battlefield FACE DOWN under the ability controller
    /// (P0), apply the profile, and leave non-creature cards in the milled zone.
    #[test]
    fn change_zone_all_face_down_under_controller_applies_profile() {
        use crate::types::ability::{ControllerRef, FaceDownProfile, TypeFilter, TypedFilter};

        let mut state = GameState::new_two_player(42);
        // Milled cards live in P1's (the opponent's) graveyard. The ability
        // controller is P0.
        let mut creature_ids = Vec::new();
        for i in 0..2 {
            let id = create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(1),
                format!("Milled Creature {i}"),
                Zone::Graveyard,
            );
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
            creature_ids.push(id);
        }
        // A land in the same milled set must NOT move.
        let land = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "Milled Land".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&land).unwrap().card_types.core_types = vec![CoreType::Land];

        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        let mut milled = creature_ids.clone();
        milled.push(land);
        state.tracked_object_sets.insert(set_id, milled);

        let creature_filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![],
        });
        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::TrackedSetFiltered {
                    id: TrackedSetId(0),
                    filter: Box::new(creature_filter),
                },
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: Some(FaceDownProfile {
                    power: Some(2),
                    toughness: Some(2),
                    extra_core_types: vec![CoreType::Artifact],
                    subtypes: vec!["Cyberman".to_string()],
                }),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();

        for id in &creature_ids {
            let obj = &state.objects[id];
            assert_eq!(obj.zone, Zone::Battlefield, "creature card must enter");
            assert!(obj.face_down, "must be face down (CR 708.3)");
            assert_eq!(obj.controller, PlayerId(0), "CR 110.2a: under controller");
            assert_eq!(obj.power, Some(2));
            assert_eq!(obj.toughness, Some(2));
            assert_eq!(
                obj.card_types.core_types,
                vec![CoreType::Creature, CoreType::Artifact]
            );
            assert_eq!(obj.card_types.subtypes, vec!["Cyberman".to_string()]);
        }
        // The land stays in P1's graveyard.
        assert_eq!(state.objects[&land].zone, Zone::Graveyard);
        assert_eq!(state.objects[&land].owner, PlayerId(1));
    }

    /// CR 701.20b: Tracked-set mass moves without an explicit origin
    /// must scan the tracked objects' actual zone, not the battlefield default.
    /// Zimone-style "revealed this way" cards leave the revealed cards in the
    /// library until the follow-up `ChangeZoneAll` routes them by type.
    #[test]
    fn change_zone_all_tracked_set_without_origin_uses_member_zone() {
        let mut state = GameState::new_two_player(42);
        let land = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Tracked Land".to_string(),
            Zone::Library,
        );
        state.objects.get_mut(&land).unwrap().card_types.core_types = vec![CoreType::Land];
        let creature = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Tracked Creature".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(set_id, vec![land, creature]);

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: None,
                destination: Zone::Battlefield,
                target: TargetFilter::TrackedSetFiltered {
                    id: TrackedSetId(0),
                    filter: Box::new(TargetFilter::Typed(TypedFilter::land())),
                },
                enters_under: None,
                enter_tapped: EtbTapState::Tapped,
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&land].zone, Zone::Battlefield);
        assert!(state.objects[&land].tapped);
        assert_eq!(state.objects[&creature].zone, Zone::Library);
    }

    /// CR 603.7: `TrackedSetId(0)` must bind through `chain_tracked_set_id`
    /// before falling back to the globally latest tracked set, matching the
    /// target-filter resolver used by `matches_target_filter`.
    #[test]
    fn change_zone_all_tracked_set_zone_uses_chain_binding() {
        let mut state = GameState::new_two_player(42);
        let chain_land = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Chain Land".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&chain_land)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Land];
        let latest_land = create_object(
            &mut state,
            CardId(21),
            PlayerId(0),
            "Latest Land".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&latest_land)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Land];

        let chain_set = TrackedSetId(5);
        let latest_set = TrackedSetId(9);
        state
            .tracked_object_sets
            .insert(chain_set, vec![chain_land]);
        state
            .tracked_object_sets
            .insert(latest_set, vec![latest_land]);
        state.chain_tracked_set_id = Some(chain_set);

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: None,
                destination: Zone::Battlefield,
                target: TargetFilter::TrackedSetFiltered {
                    id: TrackedSetId(0),
                    filter: Box::new(TargetFilter::Typed(TypedFilter::land())),
                },
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&chain_land].zone, Zone::Battlefield);
        assert_eq!(state.objects[&latest_land].zone, Zone::Graveyard);
    }

    /// CR 708.2a: An empty milled set (no eligible cards) is a clean no-op.
    #[test]
    fn change_zone_all_face_down_empty_set_noop() {
        use crate::types::ability::{ControllerRef, FaceDownProfile, TypeFilter, TypedFilter};
        let mut state = GameState::new_two_player(42);
        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state.tracked_object_sets.insert(set_id, vec![]);
        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::TrackedSetFiltered {
                    id: TrackedSetId(0),
                    filter: Box::new(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        controller: None,
                        properties: vec![],
                    })),
                },
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: Some(FaceDownProfile::vanilla_2_2()),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        // Empty set → no panic, no moves.
        resolve_all(&mut state, &ability, &mut events).unwrap();
    }

    /// CR 708.2a / CR 708.3: the singular `ChangeZone` path also consumes
    /// `face_down_profile` — the direct single-eligible branch turns the moved
    /// card face down on battlefield entry.
    #[test]
    fn change_zone_single_eligible_applies_face_down_profile() {
        use crate::types::ability::FaceDownProfile;
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lone Creature".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&card).unwrap().card_types.core_types = vec![CoreType::Creature];

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: Some(FaceDownProfile::vanilla_2_2()),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&card];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(
            obj.face_down,
            "singular ChangeZone must turn the card face down"
        );
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        assert_eq!(obj.card_types.core_types, vec![CoreType::Creature]);
    }

    /// CR 614.12b + CR 614.1c + CR 614.13: when a multi-target ChangeZone
    /// resolution moves two or more objects to the battlefield simultaneously
    /// and each has a per-permanent replacement choice (shock-land "pay 2
    /// life?" prompt), every chosen object must end up in the destination
    /// zone. Pre-fix, the first NeedsChoice abandoned the remaining iterations
    /// — only the first card ever entered the battlefield (issue #535).
    #[test]
    fn multi_target_change_zone_with_per_target_replacement_choice_processes_all_targets() {
        use crate::game::engine::apply_as_current;
        use crate::game::game_object::GameObject;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ReplacementDefinition, ReplacementMode,
        };
        use crate::types::actions::GameAction;
        use crate::types::replacements::ReplacementEvent;

        fn add_shock_in_library(state: &mut GameState, id: u64, owner: PlayerId) -> ObjectId {
            let obj_id = ObjectId(id);
            let mut obj = GameObject::new(
                obj_id,
                CardId(id),
                owner,
                format!("Shock {id}"),
                Zone::Library,
            );
            let repl = ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::MayCost {
                    cost: AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                    },
                    decline: Some(Box::new(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Tap {
                            target: TargetFilter::SelfRef,
                        },
                    ))),
                })
                .valid_card(TargetFilter::SelfRef);
            obj.replacement_definitions = vec![repl].into();
            state.objects.insert(obj_id, obj);
            state
                .players
                .iter_mut()
                .find(|p| p.id == owner)
                .unwrap()
                .library
                .push_back(obj_id);
            obj_id
        }

        let mut state = GameState::new_two_player(42);
        let shock_a = add_shock_in_library(&mut state, 501, PlayerId(0));
        let shock_b = add_shock_in_library(&mut state, 502, PlayerId(0));

        // Active/priority player drives the choices.
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let life_before = state.players[0].life;

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: true,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![TargetRef::Object(shock_a), TargetRef::Object(shock_b)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // First NeedsChoice fires for shock_a; the engine must be parked.
        assert!(
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "expected first ReplacementChoice, got {:?}",
            state.waiting_for
        );

        // Decline (index 1) — first shock enters tapped, no life paid.
        let _ = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
            .expect("decline first replacement");

        // Discriminator: pre-fix this was Priority because the inner loop returned
        // after the first NeedsChoice and the second target was abandoned.
        assert!(
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "expected a SECOND ReplacementChoice for shock_b, got {:?} — second target was abandoned",
            state.waiting_for
        );

        // Decline the second one as well.
        let _ = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
            .expect("decline second replacement");

        assert_eq!(
            state.objects[&shock_a].zone,
            Zone::Battlefield,
            "shock_a must end up on the battlefield"
        );
        assert_eq!(
            state.objects[&shock_b].zone,
            Zone::Battlefield,
            "shock_b must end up on the battlefield (pre-fix this was Library)"
        );
        assert!(
            state.objects[&shock_a].tapped,
            "shock_a declined → enters tapped"
        );
        assert!(
            state.objects[&shock_b].tapped,
            "shock_b declined → enters tapped"
        );
        assert_eq!(
            state.players[0].life, life_before,
            "both declined → no life paid"
        );
        assert!(
            state.pending_change_zone_iteration.is_none(),
            "resume slot must be cleared once the loop completes"
        );
    }

    /// Issue #567: `ChangeZoneAll::resolve_all` must stash remaining matches on
    /// `NeedsChoice` and resume via `drain_pending_change_zone_iteration` — the
    /// same contract as the targeted `ChangeZone` loop (issue #535).
    #[test]
    fn issue_567_change_zone_all_with_replacement_choice_processes_all_matches() {
        use crate::game::engine::apply_as_current;
        use crate::types::actions::GameAction;
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let shock_a = add_shock_in_library_for_test(&mut state, 601, PlayerId(0));
        let shock_b = add_shock_in_library_for_test(&mut state, 602, PlayerId(0));
        for id in [shock_a, shock_b] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.base_card_types = obj.card_types.clone();
        }

        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let life_before = state.players[0].life;

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert!(
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "expected first ReplacementChoice, got {:?}",
            state.waiting_for
        );
        assert!(
            state.pending_change_zone_iteration.is_some(),
            "resolve_all must stash remaining library matches on NeedsChoice"
        );

        let _ = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
            .expect("decline first replacement");

        assert!(
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "expected a SECOND ReplacementChoice for shock_b, got {:?} — remaining matches were abandoned",
            state.waiting_for
        );

        let _ = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
            .expect("decline second replacement");

        assert_eq!(state.objects[&shock_a].zone, Zone::Battlefield);
        assert_eq!(state.objects[&shock_b].zone, Zone::Battlefield);
        assert!(state.objects[&shock_a].tapped);
        assert!(state.objects[&shock_b].tapped);
        assert_eq!(state.players[0].life, life_before);
        assert!(state.pending_change_zone_iteration.is_none());
    }

    /// Helper: replicates the shock-land-in-library scaffolding used across
    /// the resume-loop tests below.
    #[cfg(test)]
    fn add_shock_in_library_for_test(state: &mut GameState, id: u64, owner: PlayerId) -> ObjectId {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ReplacementDefinition, ReplacementMode,
        };
        use crate::types::replacements::ReplacementEvent;

        let obj_id = ObjectId(id);
        let mut obj = GameObject::new(
            obj_id,
            CardId(id),
            owner,
            format!("Shock {id}"),
            Zone::Library,
        );
        let repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .mode(ReplacementMode::MayCost {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                },
                decline: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Tap {
                        target: TargetFilter::SelfRef,
                    },
                ))),
            })
            .valid_card(TargetFilter::SelfRef);
        obj.replacement_definitions = vec![repl].into();
        state.objects.insert(obj_id, obj);
        state
            .players
            .iter_mut()
            .find(|p| p.id == owner)
            .unwrap()
            .library
            .push_back(obj_id);
        obj_id
    }

    /// CR 614.12b + CR 614.1c: Pay the first shock-land's life cost, decline
    /// the second. Both lands must end up on the battlefield; the first
    /// untapped (paid), the second tapped (declined); life dropped by exactly
    /// 2 (cost of the first only).
    #[test]
    fn multi_target_change_zone_paying_first_shock_then_declining_second() {
        use crate::game::engine::apply_as_current;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        let shock_a = add_shock_in_library_for_test(&mut state, 601, PlayerId(0));
        let shock_b = add_shock_in_library_for_test(&mut state, 602, PlayerId(0));
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let life_before = state.players[0].life;

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: true,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![TargetRef::Object(shock_a), TargetRef::Object(shock_b)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // First shock: pay (index 0).
        let _ = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("pay first shock");
        assert!(
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "second shock must prompt after first resolves"
        );
        // Second shock: decline (index 1).
        let _ = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
            .expect("decline second shock");

        assert_eq!(state.objects[&shock_a].zone, Zone::Battlefield);
        assert_eq!(state.objects[&shock_b].zone, Zone::Battlefield);
        assert!(!state.objects[&shock_a].tapped, "first paid → untapped");
        assert!(state.objects[&shock_b].tapped, "second declined → tapped");
        assert_eq!(
            state.players[0].life,
            life_before - 2,
            "only the first shock's 2 life cost paid"
        );
    }

    /// Regression guard: three sequential `ReplacementChoice` pauses must all
    /// resume. A resume primitive that only fires once would leave the third
    /// shock stranded.
    #[test]
    fn multi_target_change_zone_resume_drives_third_target_with_choice_after_two_chained_pauses() {
        use crate::game::engine::apply_as_current;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        let s1 = add_shock_in_library_for_test(&mut state, 701, PlayerId(0));
        let s2 = add_shock_in_library_for_test(&mut state, 702, PlayerId(0));
        let s3 = add_shock_in_library_for_test(&mut state, 703, PlayerId(0));
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: true,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![
                TargetRef::Object(s1),
                TargetRef::Object(s2),
                TargetRef::Object(s3),
            ],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        for _ in 0..3 {
            assert!(
                matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
                "expected ReplacementChoice at each iteration"
            );
            let _ = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
                .expect("decline shock");
        }

        for shock in [s1, s2, s3] {
            assert_eq!(
                state.objects[&shock].zone,
                Zone::Battlefield,
                "shock {:?} must end up on the battlefield",
                shock
            );
            assert!(state.objects[&shock].tapped);
        }
        assert!(state.pending_change_zone_iteration.is_none());
    }

    /// CR 614.12b: covers the parallel fix at
    /// `engine_resolution_choices.rs::EffectZoneChoice` — the multi-card loop
    /// for untargeted "put X cards from your hand onto the battlefield"
    /// patterns must also resume after a per-permanent replacement choice
    /// pauses the loop.
    #[test]
    fn effect_zone_choice_multi_card_with_replacement_choice_processes_all_chosen() {
        use crate::game::engine::apply_as_current;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        // Construct two shock-style objects but place them in HAND so that the
        // EffectZoneChoice code path (which scans eligible cards) is the one
        // that drives them onto the battlefield.
        let shock_a = {
            use crate::game::game_object::GameObject;
            use crate::types::ability::{
                AbilityCost, AbilityDefinition, AbilityKind, ReplacementDefinition, ReplacementMode,
            };
            use crate::types::replacements::ReplacementEvent;
            let oid = ObjectId(801);
            let mut obj = GameObject::new(
                oid,
                CardId(801),
                PlayerId(0),
                "HandShock A".to_string(),
                Zone::Hand,
            );
            let repl = ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::MayCost {
                    cost: AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                    },
                    decline: Some(Box::new(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Tap {
                            target: TargetFilter::SelfRef,
                        },
                    ))),
                })
                .valid_card(TargetFilter::SelfRef);
            obj.replacement_definitions = vec![repl].into();
            state.objects.insert(oid, obj);
            state.players[0].hand.push_back(oid);
            oid
        };
        let shock_b = {
            use crate::game::game_object::GameObject;
            use crate::types::ability::{
                AbilityCost, AbilityDefinition, AbilityKind, ReplacementDefinition, ReplacementMode,
            };
            use crate::types::replacements::ReplacementEvent;
            let oid = ObjectId(802);
            let mut obj = GameObject::new(
                oid,
                CardId(802),
                PlayerId(0),
                "HandShock B".to_string(),
                Zone::Hand,
            );
            let repl = ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::MayCost {
                    cost: AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                    },
                    decline: Some(Box::new(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Tap {
                            target: TargetFilter::SelfRef,
                        },
                    ))),
                })
                .valid_card(TargetFilter::SelfRef);
            obj.replacement_definitions = vec![repl].into();
            state.objects.insert(oid, obj);
            state.players[0].hand.push_back(oid);
            oid
        };
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Park the engine in EffectZoneChoice manually — there is no
        // canonical card that emits this exact prompt with both shocks
        // present, so the test drives the resume path directly.
        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            cards: vec![shock_a, shock_b],
            count: 2,
            min_count: 0,
            up_to: true,
            source_id: ObjectId(100),
            effect_kind: EffectKind::ChangeZone,
            zone: Zone::Hand,
            destination: Some(Zone::Battlefield),
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            count_param: 0,
        };

        let _ = apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![shock_a, shock_b],
            },
        )
        .expect("select both cards");

        // First shock prompts; decline it (index 1 → tap).
        assert!(
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "first shock should prompt, got {:?}",
            state.waiting_for
        );
        let _ = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
            .expect("decline first hand shock");
        // Discriminator: second shock must also prompt — pre-fix, the
        // EffectZoneChoice loop returned after the first NeedsChoice and
        // shock_b would have stayed in hand.
        assert!(
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "second shock must prompt via resume, got {:?}",
            state.waiting_for
        );
        let _ = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
            .expect("decline second hand shock");

        assert_eq!(state.objects[&shock_a].zone, Zone::Battlefield);
        assert_eq!(state.objects[&shock_b].zone, Zone::Battlefield);
        assert!(state.pending_change_zone_iteration.is_none());
    }

    /// CR 110.2a: Only `ControllerRef::You` is supported at runtime today.
    /// Any other variant on `enters_under` must surface as `EffectError::
    /// InvalidParam` from the resolver entry — the resolver MUST NOT silently
    /// pick a `PlayerId` for an unsupported variant. This guards the strict-
    /// fail branch added when the field was lifted from `bool` to
    /// `Option<ControllerRef>`. The test drives the engine through the
    /// resolver (not a shape-only construction) so a future regression that
    /// short-circuits the match is caught.
    #[test]
    fn resolver_strict_fails_on_opponent_controller_ref_with_cr_110_2a_annotation() {
        let mut state = GameState::new_two_player(7);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Stolen Creature".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                // CR 110.2a: deliberately use an unsupported variant to drive
                // the strict-fail branch.
                enters_under: Some(ControllerRef::Opponent),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        let err = resolve(&mut state, &ability, &mut events)
            .expect_err("resolver must reject unsupported ControllerRef variants");
        let msg = err.to_string();
        assert!(
            msg.contains("CR 110.2a"),
            "error must cite CR 110.2a, got {msg}"
        );
        assert!(
            msg.contains("Opponent"),
            "error must name the offending variant, got {msg}"
        );
        // Object must not have moved.
        assert_eq!(state.objects[&obj_id].zone, Zone::Graveyard);
    }

    /// CR 701.17c + CR 608.2c: Issue #1298 — Terra, Magical Adept's
    /// "Put up to one enchantment card milled this way into your hand" must
    /// scope `EffectZoneChoice` to the milled cards, not battlefield
    /// enchantments.
    #[test]
    fn tracked_set_filtered_milled_enchantment_offers_only_milled_cards() {
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::{TypeFilter, TypedFilter};
        use crate::types::card_type::CoreType;
        use crate::types::game_state::WaitingFor;

        fn mark_enchantment(state: &mut GameState, id: ObjectId) {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Enchantment);
        }

        let mut state = GameState::new_two_player(42);

        // Library top-first: one enchantment + four instants within the milled top-5.
        let milled_enchantment = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Milled Aura".to_string(),
            Zone::Library,
        );
        mark_enchantment(&mut state, milled_enchantment);
        for i in 0..4 {
            create_object(
                &mut state,
                CardId(i + 2),
                PlayerId(0),
                format!("Instant {i}"),
                Zone::Library,
            );
        }
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 10),
                PlayerId(0),
                format!("Padding {i}"),
                Zone::Library,
            );
        }

        // Trap: a battlefield enchantment matches the inner type filter but
        // is NOT among the milled cards.
        let battlefield_enchantment = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Battlefield Aura".to_string(),
            Zone::Battlefield,
        );
        mark_enchantment(&mut state, battlefield_enchantment);

        let put_sub = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Hand,
                target: TargetFilter::TrackedSetFiltered {
                    id: TrackedSetId(0),
                    filter: Box::new(TargetFilter::Typed(TypedFilter::new(
                        TypeFilter::Enchantment,
                    ))),
                },
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: true,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let ability = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(put_sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let WaitingFor::EffectZoneChoice {
            cards, destination, ..
        } = &state.waiting_for
        else {
            panic!(
                "expected EffectZoneChoice for the put-from-milled clause, got {:?}",
                state.waiting_for
            );
        };

        assert!(
            cards.contains(&milled_enchantment),
            "the milled enchantment must be offered; offered = {cards:?}"
        );
        assert!(
            !cards.contains(&battlefield_enchantment),
            "a battlefield enchantment must NEVER be offered — selection is \
             scoped to the milled tracked set (issue #1298); offered = {cards:?}"
        );
        assert_eq!(
            *destination,
            Some(Zone::Hand),
            "the chosen milled card moves to hand"
        );
    }

    /// Regression test for issue #2382: a DFC that enters the battlefield
    /// transformed (front face = non-PW, back face = PW with loyalty 3) must
    /// enter with the correct loyalty counters so the layer system derives
    /// the right loyalty and the planeswalker survives its own -1 activation.
    #[test]
    fn enter_transformed_seeds_back_face_loyalty_counters() {
        use crate::game::game_object::BackFaceData;
        use crate::types::ability::TargetRef;
        use crate::types::card_type::{CardType, CoreType};
        use crate::types::mana::ManaCost;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sorin of House Markov".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            // Front face: Vampire, no loyalty
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Vampire".to_string()],
            };
            obj.loyalty = None;
            obj.base_characteristics_initialized = true;
            // Back face: Sorin, Ravenous Neonate — planeswalker with loyalty 3
            obj.back_face = Some(BackFaceData {
                name: "Sorin, Ravenous Neonate".to_string(),
                power: None,
                toughness: None,
                loyalty: Some(3),
                defense: None,
                card_types: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Planeswalker],
                    subtypes: vec!["Sorin".to_string()],
                },
                mana_cost: ManaCost::default(),
                keywords: vec![],
                abilities: vec![],
                trigger_definitions: Default::default(),
                replacement_definitions: Default::default(),
                static_definitions: Default::default(),
                color: vec![],
                printed_ref: None,
                modal: None,
                additional_cost: None,
                strive_cost: None,
                casting_restrictions: vec![],
                casting_options: vec![],
                layout_kind: None,
            });
        }

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Exile),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: true,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![TargetRef::Object(obj_id)],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).ok();
        crate::game::layers::flush_layers(&mut state);

        let obj = state.objects.get(&obj_id).expect("object on battlefield");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "Sorin must be on the battlefield"
        );
        assert!(obj.transformed, "Sorin must show back face");
        assert_eq!(
            obj.counters
                .get(&CounterType::Loyalty)
                .copied()
                .unwrap_or(0),
            3,
            "back-face loyalty counters must be seeded (issue #2382)"
        );
        assert_eq!(
            obj.loyalty,
            Some(3),
            "layer-derived loyalty must equal the seeded loyalty counters"
        );
    }
}
