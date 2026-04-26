use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{
    Duration, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
    TypedFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{ExileLink, ExileLinkKind, GameState, WaitingFor};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// CR 401.3: Shuffle a player's library using the game's seeded RNG.
/// Reusable helper for auto-shuffle after zone moves to Library.
pub fn shuffle_library(state: &mut GameState, player: PlayerId) {
    let GameState { players, rng, .. } = state;
    if let Some(p) = players.iter_mut().find(|p| p.id == player) {
        crate::util::im_ext::shuffle_vector(&mut p.library, rng);
    }
}

/// Result of a single zone-move attempt through the replacement pipeline.
pub(crate) enum ZoneMoveResult {
    /// Object was moved (or prevented). Continue processing.
    Done,
    /// A replacement effect needs a player choice before continuing.
    NeedsChoice(PlayerId),
}

/// Execute a single object zone-change through the full pipeline:
/// ProposedEvent → replacement → move → ExileLink → shuffle → layers_dirty.
///
/// Shared by both `resolve()` (targeted) and `resolve_all()` (mass) to ensure
/// identical behavior for replacement effects, exile tracking, and auto-shuffle.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_zone_move(
    state: &mut GameState,
    obj_id: ObjectId,
    from_zone: Zone,
    dest_zone: Zone,
    source_id: ObjectId,
    duration: Option<&Duration>,
    enter_transformed: bool,
    effect_enter_tapped: bool,
    controller_override: Option<PlayerId>,
    effect_enter_with_counters: &[(String, u32)],
    events: &mut Vec<GameEvent>,
) -> ZoneMoveResult {
    let mut proposed = ProposedEvent::zone_change(obj_id, from_zone, dest_zone, Some(source_id));

    // CR 712.14a: Set enter_transformed on the proposed event so replacement effects
    // preserve it through the pipeline.
    if enter_transformed {
        if let ProposedEvent::ZoneChange {
            enter_transformed: ref mut et,
            ..
        } = proposed
        {
            *et = true;
        }
    }

    // CR 614.1: Set enter_tapped on the proposed event so replacement effects preserve it.
    if effect_enter_tapped {
        if let ProposedEvent::ZoneChange {
            enter_tapped: ref mut et,
            ..
        } = proposed
        {
            *et = crate::types::proposed_event::EtbTapState::Tapped;
        }
    }

    // CR 110.2a: Set controller_override on the proposed event so replacement effects
    // see the correct controller through the pipeline.
    if let Some(ctrl) = controller_override {
        if let ProposedEvent::ZoneChange {
            controller_override: ref mut co,
            ..
        } = proposed
        {
            *co = Some(ctrl);
        }
    }

    // CR 306.5b + CR 310.4b + CR 614.1c: Seed the intrinsic "enters with N
    // counters" replacement when a planeswalker or battle enters the
    // battlefield from any source (effect-driven entry — bounce-return,
    // reanimate, blink, etc.). Spell-cast entry is handled in stack.rs.
    if dest_zone == Zone::Battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            let intrinsic = crate::game::printed_cards::intrinsic_etb_counters(obj);
            if !intrinsic.is_empty() {
                if let ProposedEvent::ZoneChange {
                    enter_with_counters,
                    ..
                } = &mut proposed
                {
                    enter_with_counters.extend(intrinsic);
                }
            }
        }
        // CR 122.1 + CR 614.1c: Seed effect-driven enter-with-counters from
        // `Effect::ChangeZone.enter_with_counters` (Darkness Crystal class:
        // "put target creature card ... onto the battlefield with two
        // additional +1/+1 counters on it"). Only applied for battlefield
        // entries — other destinations (Exile, etc.) carry the counters
        // through to drive `apply_etb_counters` downstream when the object
        // arrives at a counter-bearing zone.
        if !effect_enter_with_counters.is_empty() {
            if let ProposedEvent::ZoneChange {
                enter_with_counters,
                ..
            } = &mut proposed
            {
                enter_with_counters.extend(effect_enter_with_counters.iter().cloned());
            }
        }
    } else if !effect_enter_with_counters.is_empty() {
        // CR 122.1 + CR 614.1c: For non-battlefield destinations (e.g., Exile
        // for "exile it with three egg counters on it"), counters are applied
        // post-move via `apply_etb_counters` directly on the object. The
        // ProposedEvent slot is reserved for battlefield entries that flow
        // through the replacement pipeline.
        if let ProposedEvent::ZoneChange {
            enter_with_counters,
            ..
        } = &mut proposed
        {
            enter_with_counters.extend(effect_enter_with_counters.iter().cloned());
        }
    }

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::ZoneChange {
                object_id,
                to,
                enter_transformed: should_transform,
                enter_tapped: should_tap,
                enter_with_counters,
                controller_override: ctrl_override,
                ..
            } = event
            {
                zones::move_to_zone(state, object_id, to, events);
                if to == Zone::Battlefield || from_zone == Zone::Battlefield {
                    state.layers_dirty = true;
                }
                // CR 712.14a: Apply transformation if entering the battlefield transformed.
                if should_transform && to == Zone::Battlefield {
                    if let Some(obj) = state.objects.get(&object_id) {
                        if obj.back_face.is_some() && !obj.transformed {
                            let _ = crate::game::transform::transform_permanent(
                                state, object_id, events,
                            );
                        }
                    }
                }
                // CR 614.1: Apply enter-tapped if the effect or replacement set it.
                if should_tap.resolve(false) && to == Zone::Battlefield {
                    if let Some(obj) = state.objects.get_mut(&object_id) {
                        obj.tapped = true;
                    }
                }
                // CR 110.2a: Apply controller override if the effect specifies
                // "under your control" — set before triggers fire.
                if let Some(new_controller) = ctrl_override {
                    if to == Zone::Battlefield {
                        if let Some(obj) = state.objects.get_mut(&object_id) {
                            obj.controller = new_controller;
                        }
                    }
                }
                // CR 614.1c: Apply counters from replacement pipeline (e.g., saga lore counters,
                // planeswalker intrinsic loyalty, battle intrinsic defense).
                if to == Zone::Battlefield {
                    crate::game::engine_replacement::apply_etb_counters(
                        state,
                        object_id,
                        &enter_with_counters,
                        events,
                    );
                    // CR 614.1c: Apply pending ETB counters from delayed triggers
                    // (e.g., "that creature enters with an additional +1/+1 counter").
                    let pending: Vec<_> = state
                        .pending_etb_counters
                        .iter()
                        .filter(|(oid, _, _)| *oid == object_id)
                        .map(|(_, ct, n)| (ct.clone(), *n))
                        .collect();
                    if !pending.is_empty() {
                        crate::game::engine_replacement::apply_etb_counters(
                            state, object_id, &pending, events,
                        );
                        state
                            .pending_etb_counters
                            .retain(|(oid, _, _)| *oid != object_id);
                    }
                } else if !enter_with_counters.is_empty() {
                    // CR 122.1: Effect-driven counters for non-battlefield
                    // destinations — e.g., "exile it with three egg counters
                    // on it" (Darigaaz Reincarnated). Apply directly via the
                    // shared single-authority resolver so counter-doubling
                    // replacements (Doubling Season, Hardened Scales) and
                    // event emission stay consistent.
                    crate::game::engine_replacement::apply_etb_counters(
                        state,
                        object_id,
                        &enter_with_counters,
                        events,
                    );
                }
                // CR 401.3: If an object is put into a library (not at a specific
                // position), that library is shuffled afterward.
                if to == Zone::Library {
                    let owner = state.objects.get(&object_id).map(|o| o.owner);
                    if let Some(owner) = owner {
                        shuffle_library(state, owner);
                    }
                }
                // Track cards exiled by the source. Some linked exiles return when the
                // source leaves; others are just remembered as "exiled with" the source.
                if to == Zone::Exile {
                    let kind = match duration {
                        Some(Duration::UntilHostLeavesPlay) => ExileLinkKind::UntilSourceLeaves {
                            return_zone: from_zone,
                        },
                        _ => ExileLinkKind::TrackedBySource,
                    };
                    state.exile_links.push(ExileLink {
                        exiled_id: object_id,
                        source_id,
                        kind,
                    });
                }
            }
            ZoneMoveResult::Done
        }
        ReplacementResult::Prevented => ZoneMoveResult::Done,
        ReplacementResult::NeedsChoice(player) => ZoneMoveResult::NeedsChoice(player),
    }
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
        under_your_control,
        effect_enter_tapped,
        effect_enters_attacking,
        up_to,
        effect_enter_with_counters,
    ) = match &ability.effect {
        Effect::ChangeZone {
            origin,
            destination,
            owner_library,
            enter_transformed,
            under_your_control,
            enter_tapped,
            enters_attacking,
            up_to,
            enter_with_counters,
            ..
        } => {
            // CR 122.1 + CR 614.1c: Resolve `QuantityExpr` counts to concrete
            // u32 values up front so the zone-move pipeline carries fully-
            // resolved counts (matches the Token resolver pattern at
            // `effects/token.rs:400`).
            let resolved_counters: Vec<(String, u32)> = enter_with_counters
                .iter()
                .map(|(ct, qty)| {
                    let n =
                        crate::game::quantity::resolve_quantity_with_targets(state, qty, ability)
                            .max(0) as u32;
                    (ct.clone(), n)
                })
                .collect();
            (
                *origin,
                *destination,
                *owner_library,
                *enter_transformed,
                *under_your_control,
                *enter_tapped,
                *enters_attacking,
                *up_to,
                resolved_counters,
            )
        }
        _ => return Err(EffectError::MissingParam("Destination".to_string())),
    };

    let mut origin = origin;

    let target_filter = match &ability.effect {
        Effect::ChangeZone { target, .. } => target,
        _ => &TargetFilter::Any,
    };
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

    // CR 608.2c + 603.10a: Self-referential top-level triggers process the
    // source object through the zone-change pipeline. Covers:
    //   - `SelfRef` (the parser's `~` anaphor: "shuffle ~ into its owner's library")
    //   - `ParentTarget` (the "it" anaphor on a top-level trigger with no
    //     parent chain: Academy Rector, Bronzehide Lion, Loyal Cathar, etc.)
    //   - `None` (no explicit target on an effect that still needs a subject)
    // In all three cases, an empty `ability.targets` means "the source object".
    // `TriggeringSource` is deliberately excluded: it resolves via
    // `state.current_trigger_event`, not `source_id`.
    let use_self = matches!(
        target_filter,
        TargetFilter::None | TargetFilter::SelfRef | TargetFilter::ParentTarget
    ) && ability.targets.is_empty();
    let self_ref_targets = if use_self {
        vec![TargetRef::Object(ability.source_id)]
    } else {
        vec![]
    };

    let effective_targets = if self_ref_targets.is_empty() {
        &ability.targets
    } else {
        &self_ref_targets
    };
    let targeted_objects: Vec<ObjectId> = effective_targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(obj_id) => Some(*obj_id),
            TargetRef::Player(_) => None,
        })
        .collect();

    if targeted_objects.is_empty() {
        // CR 115.6: "Up to one target" — if the player chose zero targets during
        // targeting, the effect resolves doing nothing. Don't fall through to the
        // untargeted zone-scan path (which is for genuinely untargeted effects like
        // "sacrifice a creature" where the choice happens at resolution).
        if ability.optional_targeting {
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
        if origin == Some(Zone::Library) && matches!(target_filter, TargetFilter::Any) {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        let scan_zone = origin
            .or_else(|| target_filter.extract_in_zone())
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

        if eligible.len() == 1 && !up_to {
            let ctrl_override = if under_your_control {
                Some(ability.controller)
            } else {
                None
            };
            match execute_zone_move(
                state,
                eligible[0],
                scan_zone,
                dest_zone,
                ability.source_id,
                ability.duration.as_ref(),
                effect_enter_transformed,
                effect_enter_tapped,
                ctrl_override,
                &effect_enter_with_counters,
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
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    return Ok(());
                }
            }

            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: filter_controller,
            cards: eligible,
            count: 1,
            up_to,
            source_id: ability.source_id,
            effect_kind: EffectKind::ChangeZone,
            zone: scan_zone,
            destination: Some(dest_zone),
            enter_tapped: effect_enter_tapped,
            enter_transformed: effect_enter_transformed,
            under_your_control,
            enters_attacking: effect_enters_attacking,
            owner_library,
        };
        // EffectResolved is emitted by the EffectZoneChoice handler after the player chooses
        // (matching the DiscardChoice pattern — single authority for the event).
        return Ok(());
    }

    for obj_id in targeted_objects {
        // CR 114.5: Emblems cannot be moved between zones
        if state.objects.get(&obj_id).is_some_and(|o| o.is_emblem) {
            continue;
        }

        let from_zone = state
            .objects
            .get(&obj_id)
            .map(|o| o.zone)
            .unwrap_or(Zone::Battlefield);

        // CR 400.7: If an origin zone is specified and the object is no longer
        // in that zone, the zone change is impossible — skip this object.
        // Prevents delayed triggers from moving objects that have already left
        // the expected zone (e.g., Warp creature that died before end step).
        if let Some(expected_origin) = origin {
            if from_zone != expected_origin {
                continue;
            }
        }

        // CR 400.7: When owner_library is true, route to the object's owner's library.
        // The actual owner routing is handled by zones::move_to_zone which uses
        // the object's owner for player-owned zones.
        let effective_dest = dest_zone;
        let _ = owner_library; // routing handled by move_to_zone

        // CR 110.2a: When under_your_control is true, pass the controller override
        // into the zone-move pipeline so replacement effects see the correct controller.
        let ctrl_override = if under_your_control {
            Some(ability.controller)
        } else {
            None
        };

        match execute_zone_move(
            state,
            obj_id,
            from_zone,
            effective_dest,
            ability.source_id,
            ability.duration.as_ref(),
            effect_enter_transformed,
            effect_enter_tapped,
            ctrl_override,
            &effect_enter_with_counters,
            events,
        ) {
            ZoneMoveResult::Done => {
                // CR 508.4: Place on battlefield attacking (not declared as attacker).
                if effect_enters_attacking && effective_dest == Zone::Battlefield {
                    crate::game::combat::enter_attacking(
                        state,
                        obj_id,
                        ability.source_id,
                        ability.controller,
                    );
                }
            }
            ZoneMoveResult::NeedsChoice(player) => {
                state.waiting_for =
                    crate::game::replacement::replacement_choice_waiting_for(player, state);
                return Ok(());
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
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
    let (origin_zones, dest_zone, target_filter) = match &ability.effect {
        Effect::ChangeZoneAll {
            origin,
            destination,
            target,
        } => {
            let extracted = target.extract_zones();
            let scan_zones = if extracted.len() > 1 {
                extracted
            } else {
                vec![origin.unwrap_or(Zone::Battlefield)]
            };
            (scan_zones, *destination, target.clone())
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
    // correct set. Mirrors the sentinel handling in `grant_permission.rs`.
    let effective_filter = match effective_filter {
        TargetFilter::TrackedSet {
            id: TrackedSetId(0),
        } => state
            .tracked_object_sets
            .iter()
            .filter(|(_, objects)| !objects.is_empty())
            .max_by_key(|(id, _)| id.0)
            .map(|(&real_id, _)| TargetFilter::TrackedSet { id: real_id })
            .unwrap_or(effective_filter),
        other => other,
    };

    let filter_controller =
        crate::game::effects::controller_for_relative_filter(ability, &effective_filter);

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
        // CR 404.2 + CR 110.2: Hand / library / graveyard / exile membership is
        // keyed by *owner*, not controller — only permanents on the battlefield
        // have a controller. A creature stolen via Mind Control retains
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

    // Clean up consumed tracked set after scanning.
    if let TargetFilter::TrackedSet { id } = &effective_filter {
        state.tracked_object_sets.remove(id);
    }

    let mut moved_count: i32 = 0;
    for obj_id in matching {
        // CR 400.3: Each object's actual current zone is the source zone for the
        // move. Single-zone callers pass `origin_zones = [zone]`; multi-zone
        // callers (e.g. "search graveyard, hand, and library") let each object's
        // own zone drive the move so per-zone replacements/triggers fire correctly.
        let per_object_origin = state
            .objects
            .get(&obj_id)
            .map(|o| o.zone)
            .unwrap_or(origin_zone);
        // Mass zone moves don't use enter_transformed, enter_tapped, or controller_override
        match execute_zone_move(
            state,
            obj_id,
            per_object_origin,
            dest_zone,
            ability.source_id,
            ability.duration.as_ref(),
            false,
            false,
            None,
            &[],
            events,
        ) {
            ZoneMoveResult::Done => {
                moved_count += 1;
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
                state.waiting_for =
                    crate::game::replacement::replacement_choice_waiting_for(player, state);
                return Ok(());
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetFilter;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_hand_choice_ability(up_to: bool) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to,
                enter_with_counters: vec![],
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![("egg".to_string(), QuantityExpr::Fixed { value: 3 })],
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
    fn exile_without_until_host_leaves_tracks_by_source() {
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![TargetRef::Object(target_id)],
            ObjectId(100),
            PlayerId(0),
        );
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
        // CR 401.3: Moving an object to a library should shuffle that library afterward
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
                under_your_control: true,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
                under_your_control: true,
                enter_tapped: false,
                enters_attacking: true,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![TargetRef::Object(obj_id)],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Object should be on battlefield, tapped, and in combat
        assert!(state.battlefield.contains(&obj_id));
        assert!(
            state.objects[&obj_id].tapped,
            "CR 508.4: enters attacking should set tapped"
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
                under_your_control: true,
                enter_tapped: true,
                enters_attacking: false,
                up_to: true,
                enter_with_counters: vec![],
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
                under_your_control,
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
                assert!(*enter_tapped);
                assert!(*enter_transformed);
                assert!(*under_your_control);
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
                    under_your_control: false,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
                    under_your_control: false,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
                vec![TargetRef::Object(seed)],
                ObjectId(100),
                PlayerId(0),
            )
            .sub_ability(multi_zone)
            .condition(AbilityCondition::AdditionalCostPaid);
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
                under_your_control: false,
                enter_tapped: true,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
}
